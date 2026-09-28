// Command terraform-provider-fake is a protocol 6 Terraform provider built on
// the Plugin Framework that reproduces provider behaviours the cuenv
// infrastructure engine must handle. It exists only for
// crates/infrastructure/tests/provider_end_to_end.rs; build it with
//
//	go build -o terraform-provider-fake .
//
// and point CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER at the binary.
//
// The provider's `directory` argument names a directory where every
// operation is journaled (journal.log) and where the presence of flag files
// changes behaviour:
//
//	create-ok     fake_taintdel creates succeed (otherwise they fail part way)
//	fail-delete   fake_taintdel deletes fail
//	upgrade-json  fake_plain upgraded states are sent as JSON
//
// Resource types:
//
//	fake_nested    nested attributes and nested blocks with computed children
//	fake_semantic  keeps the prior name when the configured one differs only in case
//	fake_taintdel  creates that fail part way; deletes that can fail
//	fake_jsonplan  planned states sent as JSON instead of MessagePack
//	fake_plain     upgraded states optionally sent as JSON
//	fake_undead    deletes that return the object instead of null, without errors
//	fake_slow      creates that wait until the provider is asked to stop
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/datasource"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/provider"
	providerschema "github.com/hashicorp/terraform-plugin-framework/provider/schema"
	"github.com/hashicorp/terraform-plugin-framework/providerserver"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/types"
	"github.com/hashicorp/terraform-plugin-framework/types/basetypes"
	"github.com/hashicorp/terraform-plugin-go/tfprotov6"
	"github.com/hashicorp/terraform-plugin-go/tfprotov6/tf6server"
	"github.com/hashicorp/terraform-plugin-go/tftypes"
)

var (
	directoryLock sync.Mutex
	directory     string
)

func setDirectory(value string) {
	directoryLock.Lock()
	defer directoryLock.Unlock()
	directory = value
}

func currentDirectory() string {
	directoryLock.Lock()
	defer directoryLock.Unlock()
	return directory
}

func journal(format string, arguments ...any) {
	base := currentDirectory()
	if base == "" {
		return
	}
	file, err := os.OpenFile(filepath.Join(base, "journal.log"), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return
	}
	defer file.Close()
	fmt.Fprintf(file, format+"\n", arguments...)
}

func flag(name string) bool {
	base := currentDirectory()
	if base == "" {
		return false
	}
	_, err := os.Stat(filepath.Join(base, name))
	return err == nil
}

type fakeProvider struct{}

type providerModel struct {
	Directory types.String `tfsdk:"directory"`
}

func (p *fakeProvider) Metadata(_ context.Context, _ provider.MetadataRequest, response *provider.MetadataResponse) {
	response.TypeName = "fake"
}

func (p *fakeProvider) Schema(_ context.Context, _ provider.SchemaRequest, response *provider.SchemaResponse) {
	response.Schema = providerschema.Schema{Attributes: map[string]providerschema.Attribute{
		"directory": providerschema.StringAttribute{Required: true},
	}}
}

func (p *fakeProvider) Configure(ctx context.Context, request provider.ConfigureRequest, response *provider.ConfigureResponse) {
	var model providerModel
	response.Diagnostics.Append(request.Config.Get(ctx, &model)...)
	setDirectory(model.Directory.ValueString())
}

func (p *fakeProvider) DataSources(context.Context) []func() datasource.DataSource { return nil }

func (p *fakeProvider) Resources(context.Context) []func() resource.Resource {
	return []func() resource.Resource{
		func() resource.Resource { return &nested{} },
		func() resource.Resource { return &simple{kind: "semantic"} },
		func() resource.Resource { return &simple{kind: "taintdel"} },
		func() resource.Resource { return &simple{kind: "jsonplan"} },
		func() resource.Resource { return &simple{kind: "plain"} },
		func() resource.Resource { return &simple{kind: "undead"} },
		func() resource.Resource { return &simple{kind: "slow"} },
	}
}

var identifierAttribute = schema.StringAttribute{
	Computed:      true,
	PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()},
}

// nested: an optional list nested attribute and a list nested block, each
// with a computed child.
type nested struct{}

type nestedModel struct {
	ID    types.String `tfsdk:"id"`
	Name  types.String `tfsdk:"name"`
	Rules types.List   `tfsdk:"rules"`
	Rule  types.List   `tfsdk:"rule"`
}

var ruleType = map[string]attr.Type{"label": types.StringType, "rule_id": types.StringType}

func ruleAttributes() map[string]schema.Attribute {
	return map[string]schema.Attribute{
		"label":   schema.StringAttribute{Required: true},
		"rule_id": schema.StringAttribute{Computed: true},
	}
}

func (r *nested) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_nested"
}

func (r *nested) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	response.Schema = schema.Schema{
		Attributes: map[string]schema.Attribute{
			"id":   identifierAttribute,
			"name": schema.StringAttribute{Required: true},
			"rules": schema.ListNestedAttribute{
				Optional:     true,
				NestedObject: schema.NestedAttributeObject{Attributes: ruleAttributes()},
			},
		},
		Blocks: map[string]schema.Block{
			"rule": schema.ListNestedBlock{NestedObject: schema.NestedBlockObject{
				Attributes: map[string]schema.Attribute{
					"label":   schema.StringAttribute{Required: true},
					"rule_id": schema.StringAttribute{Computed: true},
				},
			}},
		},
	}
}

func computeRules(list types.List, prefix string) types.List {
	if list.IsNull() || list.IsUnknown() {
		return list
	}
	var elements []attr.Value
	for _, element := range list.Elements() {
		object := element.(basetypes.ObjectValue)
		label := object.Attributes()["label"].(types.String).ValueString()
		elements = append(elements, types.ObjectValueMust(ruleType, map[string]attr.Value{
			"label":   types.StringValue(label),
			"rule_id": types.StringValue(prefix + label),
		}))
	}
	return types.ListValueMust(types.ObjectType{AttrTypes: ruleType}, elements)
}

func (r *nested) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model nestedModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	model.ID = types.StringValue("nested-1")
	model.Rules = computeRules(model.Rules, "attribute-")
	model.Rule = computeRules(model.Rule, "block-")
	journal("nested: Create")
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *nested) Read(context.Context, resource.ReadRequest, *resource.ReadResponse) {}

func (r *nested) Update(ctx context.Context, request resource.UpdateRequest, response *resource.UpdateResponse) {
	var model nestedModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	model.Rules = computeRules(model.Rules, "attribute-")
	model.Rule = computeRules(model.Rule, "block-")
	journal("nested: Update")
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *nested) Delete(context.Context, resource.DeleteRequest, *resource.DeleteResponse) {
	journal("nested: Delete")
}

// simple resources: id, name and a computed arn.
type simple struct{ kind string }

type simpleModel struct {
	ID   types.String `tfsdk:"id"`
	Name types.String `tfsdk:"name"`
	Arn  types.String `tfsdk:"arn"`
}

func (r *simple) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_" + r.kind
}

func (r *simple) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"id":   identifierAttribute,
		"name": schema.StringAttribute{Required: true},
		"arn": schema.StringAttribute{
			Computed:      true,
			PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()},
		},
	}}
}

// ModifyPlan keeps the prior name when the configured one differs only in
// case, as a custom type with semantic equality would.
func (r *simple) ModifyPlan(ctx context.Context, request resource.ModifyPlanRequest, response *resource.ModifyPlanResponse) {
	if r.kind != "semantic" || request.State.Raw.IsNull() || request.Plan.Raw.IsNull() {
		return
	}
	var prior, planned types.String
	response.Diagnostics.Append(request.State.GetAttribute(ctx, path.Root("name"), &prior)...)
	response.Diagnostics.Append(request.Plan.GetAttribute(ctx, path.Root("name"), &planned)...)
	if strings.EqualFold(prior.ValueString(), planned.ValueString()) {
		response.Diagnostics.Append(response.Plan.SetAttribute(ctx, path.Root("name"), prior)...)
	}
}

func (r *simple) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model simpleModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	journal("%s: Create name=%s", r.kind, model.Name.ValueString())
	if r.kind == "taintdel" && !flag("create-ok") {
		response.Diagnostics.Append(response.State.SetAttribute(ctx, path.Root("id"), "half-created")...)
		response.Diagnostics.Append(response.State.SetAttribute(ctx, path.Root("name"), model.Name)...)
		response.Diagnostics.AddError("waiting for resource to become ready", "timeout after the remote object was created")
		return
	}
	if r.kind == "slow" {
		select {
		case <-ctx.Done():
			journal("slow: Create stopped")
			response.Diagnostics.Append(response.State.SetAttribute(ctx, path.Root("id"), "partial")...)
			response.Diagnostics.Append(response.State.SetAttribute(ctx, path.Root("name"), model.Name)...)
			response.Diagnostics.AddError("create interrupted", "the provider was asked to stop")
			return
		case <-time.After(60 * time.Second):
		}
	}
	model.ID = types.StringValue(r.kind + "-1")
	model.Arn = types.StringValue("arn:" + r.kind)
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *simple) Read(context.Context, resource.ReadRequest, *resource.ReadResponse) {}

func (r *simple) Update(ctx context.Context, request resource.UpdateRequest, response *resource.UpdateResponse) {
	var model simpleModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	journal("%s: Update name=%s", r.kind, model.Name.ValueString())
	if model.Arn.IsUnknown() {
		model.Arn = types.StringValue("arn:" + r.kind)
	}
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *simple) Delete(ctx context.Context, request resource.DeleteRequest, response *resource.DeleteResponse) {
	var model simpleModel
	response.Diagnostics.Append(request.State.Get(ctx, &model)...)
	journal("%s: Delete id=%s", r.kind, model.ID.ValueString())
	if r.kind == "taintdel" && flag("fail-delete") {
		response.Diagnostics.AddError("delete failed", "the remote API refused the delete")
	}
}

// wrap adjusts protocol messages the Plugin Framework never produces.
type wrap struct{ tfprotov6.ProviderServer }

var simpleType = tftypes.Object{AttributeTypes: map[string]tftypes.Type{
	"id": tftypes.String, "name": tftypes.String, "arn": tftypes.String,
}}

func toJSON(value *tfprotov6.DynamicValue) (*tfprotov6.DynamicValue, bool) {
	if value == nil {
		return value, false
	}
	decoded, err := value.Unmarshal(simpleType)
	if err != nil || decoded.IsNull() || !decoded.IsFullyKnown() {
		return value, false
	}
	var attributes map[string]tftypes.Value
	if err := decoded.As(&attributes); err != nil {
		return value, false
	}
	out := map[string]any{}
	for name, attribute := range attributes {
		if attribute.IsNull() {
			out[name] = nil
			continue
		}
		var text string
		_ = attribute.As(&text)
		out[name] = text
	}
	encoded, _ := json.Marshal(out)
	return &tfprotov6.DynamicValue{JSON: encoded}, true
}

func (w *wrap) PlanResourceChange(ctx context.Context, request *tfprotov6.PlanResourceChangeRequest) (*tfprotov6.PlanResourceChangeResponse, error) {
	response, err := w.ProviderServer.PlanResourceChange(ctx, request)
	if err == nil && request.TypeName == "fake_jsonplan" {
		var converted bool
		response.PlannedState, converted = toJSON(response.PlannedState)
		journal("jsonplan: PlanResourceChange planned_state sent as JSON=%v", converted)
	}
	return response, err
}

func (w *wrap) ApplyResourceChange(ctx context.Context, request *tfprotov6.ApplyResourceChangeRequest) (*tfprotov6.ApplyResourceChangeResponse, error) {
	if request.TypeName == "fake_jsonplan" {
		isNull, _ := request.PlannedState.IsNull()
		journal("jsonplan: ApplyResourceChange received planned_state null=%v", isNull)
	}
	response, err := w.ProviderServer.ApplyResourceChange(ctx, request)
	if err == nil && request.TypeName == "fake_undead" {
		if plannedNull, _ := request.PlannedState.IsNull(); plannedNull {
			journal("undead: ApplyResourceChange returned the prior state for a delete")
			response.NewState = request.PriorState
		}
	}
	return response, err
}

func (w *wrap) UpgradeResourceState(ctx context.Context, request *tfprotov6.UpgradeResourceStateRequest) (*tfprotov6.UpgradeResourceStateResponse, error) {
	response, err := w.ProviderServer.UpgradeResourceState(ctx, request)
	if err == nil && request.TypeName == "fake_plain" && flag("upgrade-json") {
		var converted bool
		response.UpgradedState, converted = toJSON(response.UpgradedState)
		journal("plain: UpgradeResourceState sent as JSON=%v", converted)
	}
	return response, err
}

func (w *wrap) ReadResource(ctx context.Context, request *tfprotov6.ReadResourceRequest) (*tfprotov6.ReadResourceResponse, error) {
	if request.TypeName == "fake_plain" {
		isNull, err := request.CurrentState.IsNull()
		journal("plain: ReadResource current_state null=%v error=%v", isNull, err)
	}
	return w.ProviderServer.ReadResource(ctx, request)
}

func main() {
	inner := providerserver.NewProtocol6(&fakeProvider{})()
	err := tf6server.Serve("example.com/test/fake", func() tfprotov6.ProviderServer { return &wrap{inner} })
	if err != nil {
		log.Fatal(err)
	}
}
