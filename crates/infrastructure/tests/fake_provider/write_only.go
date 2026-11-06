package main

import (
	"context"
	"fmt"

	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/types"
	"github.com/hashicorp/terraform-plugin-go/tfprotov6"
	"github.com/hashicorp/terraform-plugin-go/tftypes"
)

type writeOnly struct{}
type writeOnlyModel struct {
	ID        types.String `tfsdk:"id"`
	Value     types.String `tfsdk:"value"`
	WriteOnly types.String `tfsdk:"value_wo"`
	Version   types.Int64  `tfsdk:"value_wo_version"`
}

func (*writeOnly) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_write_only"
}
func (*writeOnly) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"id":               identifierAttribute,
		"value":            schema.StringAttribute{Optional: true},
		"value_wo":         schema.StringAttribute{Optional: true, WriteOnly: true},
		"value_wo_version": schema.Int64Attribute{Optional: true},
	}}
}
func (*writeOnly) ValidateConfig(ctx context.Context, request resource.ValidateConfigRequest, response *resource.ValidateConfigResponse) {
	var secret types.String
	response.Diagnostics.Append(request.Config.GetAttribute(ctx, path.Root("value_wo"), &secret)...)
	journal("write_only: Validate received=%t", !secret.IsNull() && !secret.IsUnknown())
	if flag("write-only-fail-validation") && !secret.IsNull() && !secret.IsUnknown() {
		response.Diagnostics.AddError("write-only validation rejected "+secret.ValueString(), "echo "+secret.ValueString())
	}
}
func (*writeOnly) ModifyPlan(ctx context.Context, request resource.ModifyPlanRequest, response *resource.ModifyPlanResponse) {
	if request.Plan.Raw.IsNull() {
		return
	}
	var secret, planned types.String
	response.Diagnostics.Append(request.Config.GetAttribute(ctx, path.Root("value_wo"), &secret)...)
	response.Diagnostics.Append(request.Plan.GetAttribute(ctx, path.Root("value_wo"), &planned)...)
	journal("write_only: Plan received=%t state_null=%t", !secret.IsNull() && !secret.IsUnknown(), planned.IsNull())
	if !planned.IsNull() {
		response.Diagnostics.AddError("write-only proposed state was not null", "")
	}
	if flag("write-only-replace") && !request.State.Raw.IsNull() {
		response.RequiresReplace = append(response.RequiresReplace, path.Root("value_wo"))
	}
}
func (*writeOnly) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var state writeOnlyModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &state)...)
	var secret types.String
	response.Diagnostics.Append(request.Config.GetAttribute(ctx, path.Root("value_wo"), &secret)...)
	journal("write_only: Create received=%t", !secret.IsNull() && !secret.IsUnknown())
	state.ID = types.StringValue("write-only-object")
	state.WriteOnly = types.StringNull()
	response.Diagnostics.Append(response.State.Set(ctx, &state)...)
}
func (*writeOnly) Update(ctx context.Context, request resource.UpdateRequest, response *resource.UpdateResponse) {
	var state writeOnlyModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &state)...)
	var secret types.String
	response.Diagnostics.Append(request.Config.GetAttribute(ctx, path.Root("value_wo"), &secret)...)
	journal("write_only: Update received=%t", !secret.IsNull() && !secret.IsUnknown())
	state.WriteOnly = types.StringNull()
	response.Diagnostics.Append(response.State.Set(ctx, &state)...)
}
func (*writeOnly) Read(context.Context, resource.ReadRequest, *resource.ReadResponse) {}
func (*writeOnly) Delete(context.Context, resource.DeleteRequest, *resource.DeleteResponse) {
	journal("write_only: Delete")
}
func (*writeOnly) ImportState(ctx context.Context, request resource.ImportStateRequest, response *resource.ImportStateResponse) {
	response.Diagnostics.Append(response.State.Set(ctx, &writeOnlyModel{
		ID: types.StringValue(request.ID), Value: types.StringNull(), WriteOnly: types.StringNull(), Version: types.Int64Value(1),
	})...)
}

var writeOnlyType = tftypes.Object{AttributeTypes: map[string]tftypes.Type{
	"id": tftypes.String, "value": tftypes.String, "value_wo": tftypes.String, "value_wo_version": tftypes.Number,
}}

// Mutate protocol results after the framework has enforced its own invariant.
func echoWriteOnly(state, configuration *tfprotov6.DynamicValue) (*tfprotov6.DynamicValue, error) {
	decoded, err := state.Unmarshal(writeOnlyType)
	if err != nil {
		return nil, err
	}
	var attributes map[string]tftypes.Value
	if err := decoded.As(&attributes); err != nil {
		return nil, err
	}
	if configuration != nil {
		config, err := configuration.Unmarshal(writeOnlyType)
		if err != nil {
			return nil, err
		}
		var configured map[string]tftypes.Value
		if err := config.As(&configured); err != nil {
			return nil, err
		}
		attributes["value_wo"] = configured["value_wo"]
	} else {
		attributes["value_wo"] = tftypes.NewValue(tftypes.String, "provider-echo-without-configuration")
	}
	if flag("write-only-unknown") {
		attributes["value_wo"] = tftypes.NewValue(tftypes.String, tftypes.UnknownValue)
	}
	value, err := tfprotov6.NewDynamicValue(writeOnlyType, tftypes.NewValue(writeOnlyType, attributes))
	if err != nil {
		return nil, fmt.Errorf("encode deliberately invalid write-only state: %w", err)
	}
	if flag("write-only-json") && !flag("write-only-unknown") {
		result, _ := toJSON(&value)
		return result, nil
	}
	return &value, nil
}
