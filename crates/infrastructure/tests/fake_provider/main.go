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
//	protect       fake_protect refuses destroy plans
//	slow-delete   fake_repl deletes take four seconds
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
//	fake_protect   destroy plans that can be refused and carry private data
//	fake_drift     creates whose result contradicts the plan
//	fake_version   resource schema version 1 when started as *-v1, else 0
//	fake_repl      a name that forces replacement; deletes that can be slow
//	fake_dyn       a computed dynamic attribute holding list(string)
//	fake_tags      an optional set of strings
//	fake_ordered   parent/dependent objects enforcing replacement ordering
//	fake_obj       real objects as files obj-<key>: key and version force replacement,
//	               mode "solo" updates and deletes need a childless object, creates
//	               refuse an existing key; failures by flag; deletes journal the private
//	               data they were sent
//	fake_obj2      the same under another type name (type changes of one resource)
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
		func() resource.Resource { return &basic{kind: "protect"} },
		func() resource.Resource { return &basic{kind: "drift"} },
		func() resource.Resource { return &basic{kind: "version"} },
		func() resource.Resource { return &basic{kind: "repl"} },
		func() resource.Resource { return &dynamic{} },
		func() resource.Resource { return &tags{} },
		func() resource.Resource { return &ordered{} },
		func() resource.Resource { return &obj{kind: "obj"} },
		func() resource.Resource { return &obj{kind: "obj2"} },
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

// basic resources: id and name, each kind with one behaviour.
type basic struct{ kind string }

type basicModel struct {
	ID   types.String `tfsdk:"id"`
	Name types.String `tfsdk:"name"`
}

// schemaVersionOne reports whether this binary serves resource schema
// version 1 for fake_version: it does when started through a name ending
// in -v1, so a test can run two schema versions of one provider.
func schemaVersionOne() bool {
	return strings.HasSuffix(os.Args[0], "-v1")
}

func (r *basic) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_" + r.kind
}

func (r *basic) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	name := schema.StringAttribute{Required: true}
	if r.kind == "repl" {
		name.PlanModifiers = []planmodifier.String{stringplanmodifier.RequiresReplace()}
	}
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"id":   identifierAttribute,
		"name": name,
	}}
	if r.kind == "version" && schemaVersionOne() {
		response.Schema.Version = 1
	}
}

// ModifyPlan of fake_protect runs for destroys too (the Plugin Framework
// advertises plan_destroy): it refuses them while the `protect` flag is
// set, and otherwise hands the delete private data.
func (r *basic) ModifyPlan(ctx context.Context, request resource.ModifyPlanRequest, response *resource.ModifyPlanResponse) {
	if r.kind != "protect" || !request.Plan.Raw.IsNull() {
		return
	}
	journal("protect: PlanResourceChange called for destroy")
	if flag("protect") {
		response.Diagnostics.AddError("deletion protection is enabled", "refusing to plan the destroy of this object")
		return
	}
	response.Diagnostics.Append(response.Private.SetKey(ctx, "destroy", []byte(`"planned"`))...)
}

func (r *basic) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model basicModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	journal("%s: Create name=%s", r.kind, model.Name.ValueString())
	model.ID = types.StringValue(r.kind + "-1")
	if r.kind == "drift" {
		// A provider bug: the result disagrees with the known planned name.
		model.Name = types.StringValue(model.Name.ValueString() + "-drifted")
	}
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *basic) Read(ctx context.Context, request resource.ReadRequest, response *resource.ReadResponse) {
	var model basicModel
	response.Diagnostics.Append(request.State.Get(ctx, &model)...)
	journal("%s: Read name=%s", r.kind, model.Name.ValueString())
}

func (r *basic) Update(ctx context.Context, request resource.UpdateRequest, response *resource.UpdateResponse) {
	var model basicModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	journal("%s: Update name=%s", r.kind, model.Name.ValueString())
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *basic) Delete(ctx context.Context, request resource.DeleteRequest, response *resource.DeleteResponse) {
	var model basicModel
	response.Diagnostics.Append(request.State.Get(ctx, &model)...)
	private, _ := request.Private.GetKey(ctx, "destroy")
	journal("%s: Delete name=%s private=%s", r.kind, model.Name.ValueString(), string(private))
	if r.kind == "repl" && flag("slow-delete") {
		time.Sleep(4 * time.Second)
		journal("repl: Delete name=%s finished", model.Name.ValueString())
	}
}

// dynamic: a computed dynamic attribute holding a list of strings.
type dynamic struct{}

type dynamicModel struct {
	ID   types.String  `tfsdk:"id"`
	Name types.String  `tfsdk:"name"`
	Data types.Dynamic `tfsdk:"data"`
}

func (r *dynamic) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_dyn"
}

func (r *dynamic) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		// Without UseStateForUnknown: the Plugin Framework marks computed
		// values unknown whenever the proposed new state differs from the
		// prior state, so a dynamic value sent back with another type
		// shows as a perpetual change.
		"id":   schema.StringAttribute{Computed: true},
		"name": schema.StringAttribute{Required: true},
		"data": schema.DynamicAttribute{Computed: true},
	}}
}

func remoteData() types.Dynamic {
	return types.DynamicValue(types.ListValueMust(types.StringType, []attr.Value{types.StringValue("a"), types.StringValue("b")}))
}

func (r *dynamic) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model dynamicModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	journal("dyn: Create")
	model.ID = types.StringValue("dyn-1")
	model.Data = remoteData()
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *dynamic) Read(ctx context.Context, request resource.ReadRequest, response *resource.ReadResponse) {
	var model dynamicModel
	response.Diagnostics.Append(request.State.Get(ctx, &model)...)
	journal("dyn: Read received data of type %T", model.Data.UnderlyingValue())
	model.Data = remoteData()
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *dynamic) Update(ctx context.Context, request resource.UpdateRequest, response *resource.UpdateResponse) {
	var model dynamicModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	journal("dyn: Update")
	model.ID = types.StringValue("dyn-1")
	model.Data = remoteData()
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *dynamic) Delete(context.Context, resource.DeleteRequest, *resource.DeleteResponse) {
	journal("dyn: Delete")
}

// tags: an optional set of strings.
type tags struct{}

type tagsModel struct {
	ID   types.String `tfsdk:"id"`
	Tags types.Set    `tfsdk:"tags"`
}

func (r *tags) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_tags"
}

func (r *tags) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"id":   identifierAttribute,
		"tags": schema.SetAttribute{Optional: true, ElementType: types.StringType},
	}}
}

func (r *tags) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model tagsModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	journal("tags: Create with %d tags", len(model.Tags.Elements()))
	model.ID = types.StringValue("tags-1")
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *tags) Read(context.Context, resource.ReadRequest, *resource.ReadResponse) {}

func (r *tags) Update(ctx context.Context, request resource.UpdateRequest, response *resource.UpdateResponse) {
	var model tagsModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *tags) Delete(context.Context, resource.DeleteRequest, *resource.DeleteResponse) {}

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
	if request.TypeName == "fake_version" && request.Version > 0 && !schemaVersionOne() {
		// SDKv2 behaviour for a state version newer than the schema: no
		// upgrader matches, and the JSON passes through, decoded against
		// the older schema, silently dropping what it does not know.
		journal("version: UpgradeResourceState from version %d with schema version 0", request.Version)
		var stored map[string]any
		_ = json.Unmarshal(request.RawState.JSON, &stored)
		passed, _ := json.Marshal(map[string]any{"id": stored["id"], "name": stored["name"]})
		return &tfprotov6.UpgradeResourceStateResponse{UpgradedState: &tfprotov6.DynamicValue{JSON: passed}}, nil
	}
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
