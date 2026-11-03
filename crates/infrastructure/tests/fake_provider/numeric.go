package main

import (
	"context"
	"math/big"

	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

// jsonNumber creates a real test resource whose protocol wrapper replaces the
// computed value with a valid JSON number outside the outbound codec range.
type jsonNumber struct{ sensitive bool }

type jsonNumberModel struct {
	ID    types.String `tfsdk:"id"`
	Value types.Number `tfsdk:"value"`
}

func (r *jsonNumber) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_json_number"
	if r.sensitive {
		response.TypeName = "fake_json_sensitive_number"
	}
}

func (r *jsonNumber) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"id":    identifierAttribute,
		"value": schema.NumberAttribute{Computed: true, Sensitive: r.sensitive},
	}}
	if schemaVersionOne() {
		response.Schema.Version = 1
	}
}

func (r *jsonNumber) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model jsonNumberModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	if response.Diagnostics.HasError() {
		return
	}
	journal("json_number: Create")
	model.ID = types.StringValue("created")
	model.Value = types.NumberValue(big.NewFloat(1))
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *jsonNumber) Read(context.Context, resource.ReadRequest, *resource.ReadResponse) {}

func (r *jsonNumber) Update(context.Context, resource.UpdateRequest, *resource.UpdateResponse) {}

func (r *jsonNumber) Delete(context.Context, resource.DeleteRequest, *resource.DeleteResponse) {}
