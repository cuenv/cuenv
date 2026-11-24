package main

import (
	"context"

	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/datasource"
	"github.com/hashicorp/terraform-plugin-framework/datasource/schema"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

// groups is a filtered list lookup shaped like Cloudflare's API token
// permission groups: even a lookup by exact name returns its matches as a
// list of objects, with no top-level id. It journals every read.
type groups struct{}

type groupsModel struct {
	Name   types.String `tfsdk:"name"`
	Result types.List   `tfsdk:"result"`
}

var groupAttributes = map[string]attr.Type{
	"id":     types.StringType,
	"name":   types.StringType,
	"scopes": types.ListType{ElemType: types.StringType},
}

var permissionGroups = []struct {
	id, name string
	scopes   []string
}{
	{"group-zone-read", "Zone Read", []string{"com.cloudflare.api.account.zone"}},
	{"group-dns-read", "DNS Read", []string{"com.cloudflare.api.account.zone"}},
	{"group-redirects-read", "Dynamic URL Redirects Read", []string{"com.cloudflare.api.account"}},
}

func (d *groups) Metadata(_ context.Context, _ datasource.MetadataRequest, response *datasource.MetadataResponse) {
	response.TypeName = "fake_groups"
}

func (d *groups) Schema(_ context.Context, _ datasource.SchemaRequest, response *datasource.SchemaResponse) {
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"name": schema.StringAttribute{Optional: true},
		"result": schema.ListNestedAttribute{Computed: true, NestedObject: schema.NestedAttributeObject{
			Attributes: map[string]schema.Attribute{
				"id":     schema.StringAttribute{Computed: true},
				"name":   schema.StringAttribute{Computed: true},
				"scopes": schema.ListAttribute{ElementType: types.StringType, Computed: true},
			},
		}},
	}}
}

func (d *groups) Read(ctx context.Context, request datasource.ReadRequest, response *datasource.ReadResponse) {
	var model groupsModel
	response.Diagnostics.Append(request.Config.Get(ctx, &model)...)
	if response.Diagnostics.HasError() {
		return
	}
	journal("groups: Read name=%s", model.Name.ValueString())
	matches := make([]attr.Value, 0)
	for _, group := range permissionGroups {
		if !model.Name.IsNull() && model.Name.ValueString() != group.name {
			continue
		}
		scopes, diagnostics := types.ListValueFrom(ctx, types.StringType, group.scopes)
		response.Diagnostics.Append(diagnostics...)
		object, diagnostics := types.ObjectValue(groupAttributes, map[string]attr.Value{
			"id":     types.StringValue(group.id),
			"name":   types.StringValue(group.name),
			"scopes": scopes,
		})
		response.Diagnostics.Append(diagnostics...)
		matches = append(matches, object)
	}
	result, diagnostics := types.ListValue(types.ObjectType{AttrTypes: groupAttributes}, matches)
	response.Diagnostics.Append(diagnostics...)
	model.Result = result
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}
