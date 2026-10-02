package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

// ordered keeps actual object files between provider processes. A dependent
// requires its configured parent; that parent cannot be deleted until the
// dependent is gone. This makes unsafe replacement ordering observable.
type ordered struct{}

type orderedModel struct {
	ID         types.String `tfsdk:"id"`
	Name       types.String `tfsdk:"name"`
	ParentName types.String `tfsdk:"parent_name"`
}

func (r *ordered) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_ordered"
}

func (r *ordered) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	replace := []planmodifier.String{stringplanmodifier.RequiresReplace()}
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"id":          identifierAttribute,
		"name":        schema.StringAttribute{Required: true, PlanModifiers: replace},
		"parent_name": schema.StringAttribute{Optional: true},
	}}
}

func orderedObject(model orderedModel) string {
	role := "parent"
	if strings.HasPrefix(model.Name.ValueString(), "dependent-") {
		role = "dependent"
	}
	return filepath.Join(currentDirectory(), "ordered-"+role)
}

func orderedContents(model orderedModel) string {
	if strings.HasPrefix(model.Name.ValueString(), "dependent-") {
		return model.ParentName.ValueString()
	}
	return model.Name.ValueString()
}

func (r *ordered) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model orderedModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	if !model.ParentName.IsNull() {
		parent, err := os.ReadFile(filepath.Join(currentDirectory(), "ordered-parent"))
		if err != nil || string(parent) != model.ParentName.ValueString() {
			response.Diagnostics.AddError("parent is unavailable", "create the replacement parent before its dependent")
			return
		}
	}
	if err := os.WriteFile(orderedObject(model), []byte(orderedContents(model)), 0o600); err != nil {
		response.Diagnostics.AddError("cannot create object", err.Error())
		return
	}
	journal("ordered: Create name=%s", model.Name.ValueString())
	model.ID = types.StringValue(model.Name.ValueString())
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *ordered) Read(context.Context, resource.ReadRequest, *resource.ReadResponse) {}

func (r *ordered) Update(ctx context.Context, request resource.UpdateRequest, response *resource.UpdateResponse) {
	if flag("fail-ordered-update") {
		var prior orderedModel
		response.Diagnostics.Append(request.State.Get(ctx, &prior)...)
		response.Diagnostics.Append(response.State.Set(ctx, &prior)...)
		response.Diagnostics.AddError("dependent update failed", "the dependent remains attached to its old parent")
		return
	}
	var model orderedModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	if err := os.WriteFile(orderedObject(model), []byte(orderedContents(model)), 0o600); err != nil {
		response.Diagnostics.AddError("cannot update object", err.Error())
		return
	}
	journal("ordered: Update name=%s", model.Name.ValueString())
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *ordered) Delete(ctx context.Context, request resource.DeleteRequest, response *resource.DeleteResponse) {
	var model orderedModel
	response.Diagnostics.Append(request.State.Get(ctx, &model)...)
	if !strings.HasPrefix(model.Name.ValueString(), "dependent-") {
		if parent, err := os.ReadFile(filepath.Join(currentDirectory(), "ordered-dependent")); err == nil && string(parent) == model.Name.ValueString() {
			response.Diagnostics.AddError("dependent still exists", "delete the old dependent before its parent")
			return
		}
	} else {
		if flag("fail-ordered-delete") {
			response.Diagnostics.AddError("dependent deletion failed", "keep both old objects recorded")
			return
		}
		if flag("slow-ordered-delete") {
			time.Sleep(2 * time.Second)
		}
	}
	if err := os.Remove(orderedObject(model)); err != nil && !os.IsNotExist(err) {
		response.Diagnostics.AddError("cannot delete object", err.Error())
		return
	}
	journal("ordered: Delete name=%s", model.Name.ValueString())
}
