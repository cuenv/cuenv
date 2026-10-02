package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"

	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

// obj: real objects are files obj-<key> holding {"parent":..,"mode":..}.
// key and version force replacement; parent and mode update in place.
// Creates refuse an existing key and a missing parent; deletes refuse while
// a child names this key as parent; updates with mode "solo" refuse while a
// child exists. Reads report a missing file as gone. Destroy plans attach
// private data that deletes journal, and create plans journal the private
// data of the object they replace (priorPrivate: what a create left behind
// under "created"). Flags fail-create-<key>,
// fail-update-<key> make those operations fail; taint-create-<key> makes a
// create leave the object behind and fail, so it is recorded tainted.
type obj struct{ kind string }

type objModel struct {
	ID      types.String `tfsdk:"id"`
	Key     types.String `tfsdk:"key"`
	Version types.String `tfsdk:"version"`
	Parent  types.String `tfsdk:"parent"`
	Mode    types.String `tfsdk:"mode"`
}

type objFile struct {
	Parent string `json:"parent"`
	Mode   string `json:"mode"`
}

func (r *obj) Metadata(_ context.Context, _ resource.MetadataRequest, response *resource.MetadataResponse) {
	response.TypeName = "fake_" + r.kind
}

func (r *obj) Schema(_ context.Context, _ resource.SchemaRequest, response *resource.SchemaResponse) {
	replace := []planmodifier.String{stringplanmodifier.RequiresReplace()}
	response.Schema = schema.Schema{Attributes: map[string]schema.Attribute{
		"id":      identifierAttribute,
		"key":     schema.StringAttribute{Required: true, PlanModifiers: replace},
		"version": schema.StringAttribute{Optional: true, PlanModifiers: replace},
		"parent":  schema.StringAttribute{Optional: true},
		"mode":    schema.StringAttribute{Optional: true},
	}}
}

func objPath(key string) string { return filepath.Join(currentDirectory(), "obj-"+key) }

func objChildren(key string) []string {
	entries, _ := os.ReadDir(currentDirectory())
	var children []string
	for _, entry := range entries {
		if !strings.HasPrefix(entry.Name(), "obj-") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(currentDirectory(), entry.Name()))
		if err != nil {
			continue
		}
		var file objFile
		if json.Unmarshal(data, &file) == nil && file.Parent == key {
			children = append(children, strings.TrimPrefix(entry.Name(), "obj-"))
		}
	}
	return children
}

func objWrite(model objModel) error {
	data, _ := json.Marshal(objFile{Parent: model.Parent.ValueString(), Mode: model.Mode.ValueString()})
	return os.WriteFile(objPath(model.Key.ValueString()), data, 0o600)
}

// ModifyPlan gives every plan private data a delete can be recognised by: a
// destroy plan marks it "planned-destroy", a create plan "planned-create"
// with the version it plans, so a replacement's delete shows whether it was
// sent the change's private data (the create plan's) or the refreshed one
// of the old object.
func (r *obj) ModifyPlan(ctx context.Context, request resource.ModifyPlanRequest, response *resource.ModifyPlanResponse) {
	if request.Plan.Raw.IsNull() {
		var prior objModel
		response.Diagnostics.Append(request.State.Get(ctx, &prior)...)
		journal("obj: PlanDestroy key=%s", prior.Key.ValueString())
		response.Diagnostics.Append(response.Private.SetKey(ctx, "destroy", []byte(`"planned-destroy-`+prior.Key.ValueString()+`"`))...)
		return
	}
	if request.State.Raw.IsNull() {
		var planned objModel
		response.Diagnostics.Append(request.Plan.Get(ctx, &planned)...)
		prior, _ := request.Private.GetKey(ctx, "created")
		journal("obj: PlanCreate key=%s priorPrivate=%s", planned.Key.ValueString(), string(prior))
		response.Diagnostics.Append(response.Private.SetKey(ctx, "destroy", []byte(`"planned-create-`+planned.Key.ValueString()+`-v`+planned.Version.ValueString()+`"`))...)
	}
}

func (r *obj) Create(ctx context.Context, request resource.CreateRequest, response *resource.CreateResponse) {
	var model objModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	key := model.Key.ValueString()
	if flag("fail-create-" + key) {
		journal("obj: Create key=%s FAILED (flag)", key)
		response.Diagnostics.AddError("create failed", "flag")
		return
	}
	if _, err := os.Stat(objPath(key)); err == nil {
		journal("obj: Create key=%s FAILED already exists", key)
		response.Diagnostics.AddError("already exists", "an object with key "+key+" already exists")
		return
	}
	if p := model.Parent.ValueString(); p != "" {
		if _, err := os.Stat(objPath(p)); err != nil {
			journal("obj: Create key=%s FAILED parent %s missing", key, p)
			response.Diagnostics.AddError("parent missing", p)
			return
		}
	}
	if err := objWrite(model); err != nil {
		response.Diagnostics.AddError("write", err.Error())
		return
	}
	journal("obj: Create key=%s parent=%s", key, model.Parent.ValueString())
	model.ID = types.StringValue(key)
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
	if flag("taint-create-" + key) {
		response.Diagnostics.Append(response.Private.SetKey(ctx, "created", []byte(`"created-`+key+`"`))...)
		response.Diagnostics.AddError("waiting for object to become ready", "timeout after the object was created")
	}
}

func (r *obj) Read(ctx context.Context, request resource.ReadRequest, response *resource.ReadResponse) {
	var model objModel
	response.Diagnostics.Append(request.State.Get(ctx, &model)...)
	if _, err := os.Stat(objPath(model.Key.ValueString())); err != nil {
		journal("obj: Read key=%s gone", model.Key.ValueString())
		response.State.RemoveResource(ctx)
	}
}

func (r *obj) Update(ctx context.Context, request resource.UpdateRequest, response *resource.UpdateResponse) {
	var model objModel
	response.Diagnostics.Append(request.Plan.Get(ctx, &model)...)
	key := model.Key.ValueString()
	if flag("fail-update-" + key) {
		journal("obj: Update key=%s FAILED (flag)", key)
		response.Diagnostics.AddError("update failed", "flag")
		return
	}
	if p := model.Parent.ValueString(); p != "" {
		if _, err := os.Stat(objPath(p)); err != nil {
			journal("obj: Update key=%s FAILED parent %s missing", key, p)
			response.Diagnostics.AddError("parent missing", p)
			return
		}
	}
	if model.Mode.ValueString() == "solo" {
		if children := objChildren(key); len(children) > 0 {
			journal("obj: Update key=%s FAILED children %v", key, children)
			response.Diagnostics.AddError("has children", strings.Join(children, ","))
			return
		}
	}
	if err := objWrite(model); err != nil {
		response.Diagnostics.AddError("write", err.Error())
		return
	}
	journal("obj: Update key=%s parent=%s mode=%s", key, model.Parent.ValueString(), model.Mode.ValueString())
	response.Diagnostics.Append(response.State.Set(ctx, &model)...)
}

func (r *obj) Delete(ctx context.Context, request resource.DeleteRequest, response *resource.DeleteResponse) {
	var model objModel
	response.Diagnostics.Append(request.State.Get(ctx, &model)...)
	key := model.Key.ValueString()
	private, _ := request.Private.GetKey(ctx, "destroy")
	if children := objChildren(key); len(children) > 0 {
		journal("obj: Delete key=%s FAILED children %v", key, children)
		response.Diagnostics.AddError("has children", strings.Join(children, ","))
		return
	}
	if err := os.Remove(objPath(key)); err != nil && !os.IsNotExist(err) {
		response.Diagnostics.AddError("remove", err.Error())
		return
	}
	journal("obj: Delete key=%s private=%s", key, string(private))
}
