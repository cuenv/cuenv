package main

import (
	"fmt"

	"cuelang.org/go/cue"
)

const (
	// These limits cap explicit Go graph traversal, not CUE's own evaluation or
	// Value.Validate work. Visit counts include repeated references.
	maxDAGValidationDepth  = 128
	maxDAGValidationVisits = 100_000
)

type dagValidationState struct {
	nodes       int
	instanceKey string
	visited     map[string]struct{}
}

func (state *dagValidationState) visit(path string, depth int) error {
	if depth > maxDAGValidationDepth {
		return fmt.Errorf("%s: DAG validation exceeded maximum nesting depth of %d", path, maxDAGValidationDepth)
	}
	state.nodes++
	if state.nodes > maxDAGValidationVisits {
		return fmt.Errorf("%s: DAG validation exceeded maximum visit count of %d", path, maxDAGValidationVisits)
	}
	return nil
}

// validateProjectDAGReferences rejects incomplete or untyped values that CUE
// can otherwise serialize as null or partial objects after schema unification.
func validateProjectDAGReferences(project cue.Value) error {
	// cue.Value.Err only detects bottom values; incomplete dependency
	// disjunctions can otherwise marshal as null or a partially constrained
	// object. The root marker scopes this check to schema.#Project users.
	projectMarker := project.LookupPath(cue.MakePath(cue.Hid("_cuenvValidatedProject", schemaPackagePath)))
	if !projectMarker.Exists() || projectMarker.Err() != nil {
		return nil
	}
	validatedProject, err := projectMarker.Bool()
	if err != nil || !validatedProject {
		return nil
	}

	rootFields := []string{"tasks", "images", "services"}
	state := &dagValidationState{
		instanceKey: fmt.Sprintf("%p", project.BuildInstance()),
		visited:     make(map[string]struct{}),
	}
	rootKinds := map[string]map[string]bool{
		"tasks":    {"task": true, "group": true},
		"images":   {"image": true},
		"services": {"service": true},
	}
	for _, field := range rootFields {
		root := project.LookupPath(cue.ParsePath(field))
		if !root.Exists() || root.Err() != nil || root.Kind() != cue.StructKind {
			continue
		}
		fields, err := root.Fields(cue.Definitions(false))
		if err != nil {
			return fmt.Errorf("read %s definitions: %w", field, err)
		}
		for fields.Next() {
			identityPath := cue.MakePath(cue.Str(field), cue.Str(fields.Label()))
			if err := validateProjectDAGNode(fields.Value(), field+"."+fields.Label(), identityPath, rootKinds[field], state, 0); err != nil {
				return err
			}
		}
	}
	return validateProjectPipelines(project, state)
}

func validateProjectDAGNode(node cue.Value, path string, identityPath cue.Path, allowedKinds map[string]bool, state *dagValidationState, depth int) error {
	if err := state.visit(path, depth); err != nil {
		return err
	}
	if node.Err() != nil {
		return fmt.Errorf("%s: invalid DAG node: %w", path, node.Err())
	}
	identity := dagValueIdentity(node, identityPath, state.instanceKey)
	if _, ok := state.visited[identity]; ok {
		return nil
	}
	state.visited[identity] = struct{}{}

	if node.Kind() == cue.ListKind {
		items, err := node.List()
		if err != nil {
			return fmt.Errorf("%s: read sequence: %w", path, err)
		}
		for index := 0; items.Next(); index++ {
			if err := validateProjectDAGNode(items.Value(), fmt.Sprintf("%s[%d]", path, index), identityPath.Append(cue.Index(index)), map[string]bool{"task": true, "group": true}, state, depth+1); err != nil {
				return err
			}
		}
		return nil
	}

	kind, ok := dagSchemaMarker(node)
	if !ok {
		return fmt.Errorf("%s: missing cuenv DAG schema marker", path)
	}
	if !allowedKinds[kind] {
		return fmt.Errorf("%s: unexpected cuenv DAG node kind %q", path, kind)
	}

	if err := validateDAGDependencies(node, path, identityPath, kind, state, depth+1); err != nil {
		return err
	}
	if kind == "service" {
		if err := validateServiceTaskFields(node, path, identityPath, state, depth+1); err != nil {
			return err
		}
	}

	if kind == "group" {
		children, err := node.Fields(cue.Definitions(false))
		if err != nil {
			return fmt.Errorf("%s: read task group: %w", path, err)
		}
		for children.Next() {
			label := children.Label()
			if isTaskGroupHeader(label) {
				continue
			}
			if err := validateProjectDAGNode(children.Value(), path+"."+label, identityPath.Append(cue.Str(label)), map[string]bool{"task": true, "group": true}, state, depth+1); err != nil {
				return err
			}
		}
	}
	return nil
}

func validateProjectPipelines(project cue.Value, state *dagValidationState) error {
	ci := project.LookupPath(cue.ParsePath("ci"))
	if !ci.Exists() {
		return nil
	}
	if err := ci.Err(); err != nil {
		return fmt.Errorf("ci: invalid CI configuration: %w", err)
	}

	pipelines := ci.LookupPath(cue.ParsePath("pipelines"))
	if !pipelines.Exists() {
		return nil
	}
	if err := pipelines.Err(); err != nil {
		return fmt.Errorf("ci.pipelines: invalid pipelines: %w", err)
	}
	pipelineFields, err := pipelines.Fields(cue.Definitions(false))
	if err != nil {
		return fmt.Errorf("ci.pipelines: read pipelines: %w", err)
	}
	for pipelineFields.Next() {
		pipelinePath := "ci.pipelines." + pipelineFields.Label()
		pipelineIdentityPath := cue.MakePath(cue.Str("ci"), cue.Str("pipelines"), cue.Str(pipelineFields.Label()))
		tasks := pipelineFields.Value().LookupPath(cue.ParsePath("tasks"))
		if !tasks.Exists() {
			continue
		}
		if err := tasks.Err(); err != nil {
			return fmt.Errorf("%s.tasks: invalid pipeline tasks: %w", pipelinePath, err)
		}
		if tasks.Kind() != cue.ListKind {
			return fmt.Errorf("%s.tasks: expected a CUE list", pipelinePath)
		}
		items, err := tasks.List()
		if err != nil {
			return fmt.Errorf("%s.tasks: read task list: %w", pipelinePath, err)
		}
		for index := 0; items.Next(); index++ {
			path := fmt.Sprintf("%s.tasks[%d]", pipelinePath, index)
			identityPath := pipelineIdentityPath.Append(cue.Str("tasks"), cue.Index(index))
			if err := validatePipelineTask(items.Value(), path, identityPath, state, 0); err != nil {
				return err
			}
		}
	}
	return nil
}

func validateServiceTaskFields(service cue.Value, path string, identityPath cue.Path, state *dagValidationState, depth int) error {
	entrypoint := service.LookupPath(cue.ParsePath("entrypoint"))
	if entrypoint.Exists() {
		if err := state.visit(path+".entrypoint", depth); err != nil {
			return err
		}
		if err := entrypoint.Validate(cue.Concrete(true), cue.Final()); err != nil {
			return fmt.Errorf("%s.entrypoint: incomplete service entrypoint: %w", path, err)
		}
		name := entrypoint.LookupPath(cue.MakePath(cue.Hid("_name", schemaPackagePath)))
		if name.Exists() {
			if err := validateDAGDependencies(entrypoint, path+".entrypoint", identityPath.Append(cue.Str("entrypoint")), "task", state, depth+1); err != nil {
				return err
			}
		}
	}

	watch := service.LookupPath(cue.ParsePath("watch"))
	if !watch.Exists() {
		return nil
	}
	if err := watch.Err(); err != nil {
		return fmt.Errorf("%s.watch: invalid watcher configuration: %w", path, err)
	}
	rebuild := watch.LookupPath(cue.ParsePath("rebuild"))
	if !rebuild.Exists() {
		return nil
	}
	if err := rebuild.Err(); err != nil {
		return fmt.Errorf("%s.watch.rebuild: invalid rebuild tasks: %w", path, err)
	}
	if rebuild.Kind() != cue.ListKind {
		return fmt.Errorf("%s.watch.rebuild: expected a CUE list", path)
	}
	items, err := rebuild.List()
	if err != nil {
		return fmt.Errorf("%s.watch.rebuild: read rebuild tasks: %w", path, err)
	}
	for index := 0; items.Next(); index++ {
		itemIdentityPath := identityPath.Append(cue.Str("watch"), cue.Str("rebuild"), cue.Index(index))
		if err := validateProjectDAGNode(items.Value(), fmt.Sprintf("%s.watch.rebuild[%d]", path, index), itemIdentityPath, map[string]bool{"task": true, "group": true}, state, depth+1); err != nil {
			return err
		}
	}
	return nil
}

func validatePipelineTask(task cue.Value, path string, identityPath cue.Path, state *dagValidationState, depth int) error {
	if err := state.visit(path, depth); err != nil {
		return err
	}
	if err := task.Err(); err != nil {
		return fmt.Errorf("%s: invalid pipeline task: %w", path, err)
	}
	if dagNodeType(task) != "matrix" {
		return validatePipelineNode(task, path, identityPath, state, depth+1)
	}
	if err := task.Validate(cue.Concrete(true), cue.Final()); err != nil {
		return fmt.Errorf("%s: incomplete matrix task: %w", path, err)
	}
	matrixTask := task.LookupPath(cue.ParsePath("task"))
	if !matrixTask.Exists() {
		return fmt.Errorf("%s.task: missing matrix task reference", path)
	}
	return validatePipelineNode(matrixTask, path+".task", identityPath.Append(cue.Str("task")), state, depth+1)
}

func validatePipelineNode(node cue.Value, path string, identityPath cue.Path, state *dagValidationState, depth int) error {
	if err := state.visit(path, depth); err != nil {
		return err
	}
	if err := node.Err(); err != nil {
		return fmt.Errorf("%s: invalid pipeline node: %w", path, err)
	}

	if kind, ok := dagSchemaMarker(node); ok {
		if kind != "task" && kind != "group" {
			return fmt.Errorf("%s: pipeline cannot run a %s", path, kind)
		}
		return validateProjectDAGNode(node, path, identityPath, map[string]bool{"task": true, "group": true}, state, depth+1)
	}

	if node.Kind() == cue.ListKind {
		if err := node.Validate(cue.Concrete(true), cue.Final()); err != nil {
			return fmt.Errorf("%s: incomplete pipeline sequence: %w", path, err)
		}
		return validateProjectDAGNode(node, path, identityPath, map[string]bool{"task": true, "group": true}, state, depth+1)
	}

	if err := node.Validate(cue.Concrete(true), cue.Final()); err != nil {
		return fmt.Errorf("%s: incomplete inline pipeline node: %w", path, err)
	}
	inlineKind := dagNodeType(node)
	if inlineKind == "" {
		inlineKind = "task"
	}
	if inlineKind != "task" && inlineKind != "group" {
		return fmt.Errorf("%s: pipeline cannot run a %s", path, inlineKind)
	}
	if err := validateDAGDependencies(node, path, identityPath, inlineKind, state, depth+1); err != nil {
		return err
	}
	if inlineKind == "group" {
		children, err := node.Fields(cue.Definitions(false))
		if err != nil {
			return fmt.Errorf("%s: read inline task group: %w", path, err)
		}
		for children.Next() {
			label := children.Label()
			if isTaskGroupHeader(label) {
				continue
			}
			if err := validateProjectDAGNode(children.Value(), path+"."+label, identityPath.Append(cue.Str(label)), map[string]bool{"task": true, "group": true}, state, depth+1); err != nil {
				return err
			}
		}
	}
	return nil
}

func validateDAGDependencies(node cue.Value, path string, identityPath cue.Path, sourceKind string, state *dagValidationState, depth int) error {
	dependsOn := node.LookupPath(cue.ParsePath("dependsOn"))
	if !dependsOn.Exists() {
		return nil
	}
	if err := dependsOn.Err(); err != nil {
		return fmt.Errorf("%s.dependsOn: invalid dependency list: %w", path, err)
	}
	if dependsOn.Kind() != cue.ListKind {
		return fmt.Errorf("%s.dependsOn: expected a CUE list", path)
	}

	allowedBySource := map[string]map[string]bool{
		"task":    {"task": true, "group": true, "image": true},
		"group":   {"task": true, "group": true},
		"image":   {"task": true, "group": true, "image": true},
		"service": {"task": true, "group": true, "image": true, "service": true},
	}
	allowedKinds, ok := allowedBySource[sourceKind]
	if !ok {
		return fmt.Errorf("%s: unknown DAG source kind %q", path, sourceKind)
	}

	items, err := dependsOn.List()
	if err != nil {
		return fmt.Errorf("%s.dependsOn: read dependency list: %w", path, err)
	}
	for index := 0; items.Next(); index++ {
		dependencyPath := identityPath.Append(cue.Str("dependsOn"), cue.Index(index))
		if err := validateDAGDependency(items.Value(), allowedKinds, sourceKind, fmt.Sprintf("%s.dependsOn[%d]", path, index), dependencyPath, state, depth+1); err != nil {
			return err
		}
	}
	return nil
}

func validateDAGDependency(dependency cue.Value, allowedKinds map[string]bool, sourceKind string, path string, identityPath cue.Path, state *dagValidationState, depth int) error {
	if err := state.visit(path, depth); err != nil {
		return err
	}
	if err := dependency.Err(); err != nil {
		return fmt.Errorf("%s: invalid DAG dependency: %w", path, err)
	}

	if dependency.Kind() == cue.ListKind {
		items, err := dependency.List()
		if err != nil {
			return fmt.Errorf("%s: read sequence dependency: %w", path, err)
		}
		for index := 0; items.Next(); index++ {
			itemIdentityPath := identityPath.Append(cue.Index(index))
			if err := validateDAGDependency(items.Value(), map[string]bool{"task": true, "group": true}, "sequence", fmt.Sprintf("%s[%d]", path, index), itemIdentityPath, state, depth+1); err != nil {
				return err
			}
		}
		return nil
	}
	kind, ok := dagSchemaMarker(dependency)
	if !ok {
		return fmt.Errorf("%s: dependency is not a validated CUE task, group, image, service, or sequence reference", path)
	}
	if !allowedKinds[kind] {
		return fmt.Errorf("%s: %s dependencies cannot target a %s", path, sourceKind, kind)
	}
	return validateProjectDAGNode(dependency, path, identityPath, allowedKinds, state, depth+1)
}

func dagValueIdentity(value cue.Value, identityPath cue.Path, instanceKey string) string {
	root, referencePath := value.ReferencePath()
	if root.Exists() {
		return fmt.Sprintf("%p:%s", root.BuildInstance(), referencePath.String())
	}
	return instanceKey + ":" + identityPath.String()
}

func dagSchemaMarker(value cue.Value) (string, bool) {
	marker := value.LookupPath(cue.MakePath(cue.Hid("_cuenvValidatedDAGNode", schemaPackagePath)))
	if !marker.Exists() || marker.Err() != nil {
		return "", false
	}
	kind, err := marker.String()
	return kind, err == nil
}

func dagNodeType(value cue.Value) string {
	typeField := value.LookupPath(cue.ParsePath("type"))
	if !typeField.Exists() || typeField.Err() != nil {
		return ""
	}
	nodeType, err := typeField.String()
	if err != nil {
		return ""
	}
	return nodeType
}

func isTaskGroupHeader(label string) bool {
	switch label {
	case "type", "dependsOn", "maxConcurrency", "description":
		return true
	default:
		return false
	}
}
