package main

import (
	"context"
	"path/filepath"

	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

// tokenSource reproduces the computed sensitive scalar returned only after a
// Cloudflare v5.28.1 tunnel-like resource is created. Replacing the source makes
// token unknown during planning; no UseStateForUnknown modifier hides this.
type tokenSource struct{}

type tokenSourceModel struct {
	ID    types.String `tfsdk:"id"`
	Name  types.String `tfsdk:"name"`
	Token types.String `tfsdk:"token"`
}

func (r *tokenSource) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_token_source"
}

func (r *tokenSource) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"id": identifierAttribute,
		"name": schema.StringAttribute{Required: true, PlanModifiers: []planmodifier.String{
			stringplanmodifier.RequiresReplace(),
		}},
		"token": schema.StringAttribute{Computed: true, Sensitive: true},
	}}
}

func (r *tokenSource) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model tokenSourceModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	if response.Diagnostics.HasError() {
		return
	}
	journal("token_source: Create name=%s", model.Name.ValueString())
	model.ID = types.StringValue("source-" + model.Name.ValueString())
	model.Token = types.StringValue("synthetic-token-" + filepath.Base(currentDirectory()) + "-" + model.Name.ValueString())
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *tokenSource) Read(context.Context, resource.ReadRequest, *resource.ReadResponse) {}

func (r *tokenSource) Update(_ context.Context, _ resource.UpdateRequest, response *resource.UpdateResponse) {
	response.Diagnostics.AddError("unexpected source update", "changing the source name must replace it")
}

func (r *tokenSource) Delete(context.Context, resource.DeleteRequest, *resource.DeleteResponse) {
	journal("token_source: Delete")
}

// tokenConsumer models the single nested attribute used by Cloudflare v5.28.1:
// config.token is a scalar input alongside an ordinary sibling. The replacement
// path is config, rather than the leaf token. Its schema deliberately does not
// mark token sensitive, so reference provenance must supply that protection.
// token-consumer-late-replace reports replacement only once token becomes known;
// token-consumer-fail-validation echoes known tokens in diagnostics, never logs.
type tokenConsumer struct{}

type tokenConsumerModel struct {
	ID     types.String `tfsdk:"id"`
	Config types.Object `tfsdk:"config"`
}

func (r *tokenConsumer) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_token_consumer"
}

func (r *tokenConsumer) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"id": identifierAttribute,
		"config": schema.SingleNestedAttribute{Required: true, Attributes: map[string]schema.Attribute{
			"token": schema.StringAttribute{Required: true},
			"label": schema.StringAttribute{Required: true},
		}},
	}}
}

func (r *tokenConsumer) ValidateConfig(ctx context.Context, request resource.ValidateConfigRequest, response *resource.ValidateConfigResponse) {
	if !flag("token-consumer-fail-validation") {
		return
	}
	var token types.String
	response.Diagnostics.Append(request.Config.GetAttribute(ctx, path.Root("config").AtName("token"), &token)...)
	if response.Diagnostics.HasError() || token.IsNull() || token.IsUnknown() {
		return
	}
	journal("token_consumer: ValidateConfig rejected known token")
	response.Diagnostics.AddError("nested token rejected: "+token.ValueString(), "validation received "+token.ValueString())
}

func (r *tokenConsumer) ModifyPlan(ctx context.Context, request resource.ModifyPlanRequest, response *resource.ModifyPlanResponse) {
	if request.State.Raw.IsNull() || request.Plan.Raw.IsNull() {
		return
	}
	var prior, planned types.String
	response.Diagnostics.Append(request.State.GetAttribute(ctx, path.Root("config").AtName("token"), &prior)...)
	response.Diagnostics.Append(request.Plan.GetAttribute(ctx, path.Root("config").AtName("token"), &planned)...)
	if response.Diagnostics.HasError() || prior.Equal(planned) {
		return
	}
	if flag("token-consumer-late-replace") && planned.IsUnknown() {
		journal("token_consumer: Plan unknown token permits update")
		return
	}
	journal("token_consumer: Plan requires replacement at config")
	response.RequiresReplace = append(response.RequiresReplace, path.Root("config"))
}

func (r *tokenConsumer) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model tokenConsumerModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	journal("token_consumer: Create")
	model.ID = types.StringValue("consumer-1")
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *tokenConsumer) Read(context.Context, resource.ReadRequest, *resource.ReadResponse) {}

func (r *tokenConsumer) Update(ctx context.Context, request resource.UpdateRequest, response *resource.UpdateResponse) {
	var model tokenConsumerModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	journal("token_consumer: Update")
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *tokenConsumer) Delete(context.Context, resource.DeleteRequest, *resource.DeleteResponse) {
	journal("token_consumer: Delete")
}
