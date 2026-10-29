package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"

	"github.com/hashicorp/terraform-plugin-framework/datasource"
	"github.com/hashicorp/terraform-plugin-framework/datasource/schema"
	"github.com/hashicorp/terraform-plugin-framework/types"
	"github.com/hashicorp/terraform-plugin-go/tfprotov6"
)

// lookup is a read-only remote lookup. Its journal establishes when reads occur
// across provider processes. lookup-value overrides its computed value so tests
// can change the remote result without changing the configuration. lookup-error
// returns both state and an error echoing a sensitive computed value: clients
// must register returned sensitive values before rendering diagnostics.
// lookup-invalid-result corrupts the numeric length while retaining a valid
// sensitive scalar, testing redaction even when type-directed decoding fails.
// lookup-map-error echoes a key inside a sensitive computed map.
type lookup struct{}

type lookupModel struct {
	Name        types.String `tfsdk:"name"`
	Value       types.String `tfsdk:"value"`
	SecretValue types.String `tfsdk:"secret_value"`
	SecretMap   types.Map    `tfsdk:"secret_map"`
	Length      types.Int64  `tfsdk:"length"`
	Found       types.Bool   `tfsdk:"found"`
}

func (d *lookup) Metadata(_ context.Context, _ datasource.MetadataRequest, response *datasource.MetadataResponse) {
	response.TypeName = "fake_lookup"
}

func (d *lookup) Schema(_ context.Context, _ datasource.SchemaRequest, response *datasource.SchemaResponse) {
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"name":         schema.StringAttribute{Required: true},
		"value":        schema.StringAttribute{Computed: true},
		"secret_value": schema.StringAttribute{Computed: true, Sensitive: true},
		"secret_map":   schema.MapAttribute{ElementType: types.StringType, Computed: true, Sensitive: true},
		"length":       schema.Int64Attribute{Computed: true},
		"found":        schema.BoolAttribute{Computed: true},
	}}
}

func (d *lookup) ValidateConfig(ctx context.Context, request datasource.ValidateConfigRequest, response *datasource.ValidateConfigResponse) {
	var model lookupModel
	response.Diagnostics.Append(request.Config.Get(ctx, &model)...)
	if model.Name.ValueString() == "invalid" {
		response.Diagnostics.AddError("invalid lookup name", "the fake lookup refuses this name during validation")
	}
}

func (d *lookup) Read(ctx context.Context, request datasource.ReadRequest, response *datasource.ReadResponse) {
	var model lookupModel
	response.Diagnostics.Append(request.Config.Get(ctx, &model)...)
	if response.Diagnostics.HasError() {
		return
	}
	name := model.Name.ValueString()
	journal("lookup: Read name=%s", name)
	value := "lookup:" + name
	if contents, err := os.ReadFile(filepath.Join(currentDirectory(), "lookup-value")); err == nil {
		value = string(contents)
	} else if !os.IsNotExist(err) {
		response.Diagnostics.AddError("lookup failed", err.Error())
		return
	}
	model.Value = types.StringValue(value)
	model.SecretValue = types.StringValue(lookupSecret())
	secretMap, diagnostics := types.MapValueFrom(ctx, types.StringType, map[string]string{lookupMapKey(): "ordinary-map-value"})
	response.Diagnostics.Append(diagnostics...)
	model.SecretMap = secretMap
	model.Length = types.Int64Value(int64(len(value)))
	model.Found = types.BoolValue(true)
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func lookupSecret() string {
	// Per-fixture identity prevents another parallel test's redaction registry
	// from accidentally hiding a failure to register this response's secret.
	return "lookup-secret-" + filepath.Base(currentDirectory())
}

func lookupMapKey() string {
	// Independent of lookupSecret: registering that scalar must not accidentally
	// redact the map key and hide a missing key traversal.
	return "lookup-map-key-" + filepath.Base(currentDirectory())
}

func (w *wrap) ReadDataSource(ctx context.Context, request *tfprotov6.ReadDataSourceRequest) (*tfprotov6.ReadDataSourceResponse, error) {
	response, err := w.ProviderServer.ReadDataSource(ctx, request)
	if err == nil && request.TypeName == "fake_lookup" && flag("lookup-invalid-result") {
		state, marshalError := json.Marshal(map[string]any{
			"name": "malformed", "value": "lookup:malformed", "found": true,
			"length": map[string]any{"invalid": true}, "secret_value": lookupSecret(),
		})
		if marshalError != nil {
			return nil, marshalError
		}
		response.State = &tfprotov6.DynamicValue{JSON: state}
		journal("lookup: ReadDataSource returned invalid length with sensitive scalar")
	}
	if err == nil && request.TypeName == "fake_lookup" && flag("lookup-error") {
		response.Diagnostics = append(response.Diagnostics, &tfprotov6.Diagnostic{
			Severity: tfprotov6.DiagnosticSeverityError,
			Summary:  "lookup failed: " + lookupSecret(),
			Detail:   "remote response included " + lookupSecret(),
		})
	}
	if err == nil && request.TypeName == "fake_lookup" && flag("lookup-map-error") {
		response.Diagnostics = append(response.Diagnostics, &tfprotov6.Diagnostic{
			Severity: tfprotov6.DiagnosticSeverityError,
			Summary:  "sensitive map key rejected: " + lookupMapKey(),
			Detail:   "remote response key was " + lookupMapKey(),
		})
	}
	return response, err
}
