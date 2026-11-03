package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"bytes"
	"encoding/json"
	"errors"
	"sort"

	"cuelang.org/go/cue"
)

// constrainedReference permits a direct reference with reference-free CUE
// constraints. Unlike descriptor lowering, specialization preserves and checks
// those constraints in CUE. Arithmetic, interpolation and alternative references
// are still unsupported.
func constrainedReference(value, instance cue.Value, depth int) (cue.Path, error) {
	if depth > 32 {
		return cue.Path{}, errors.New("reference nesting limit exceeded")
	}
	root, path := value.ReferencePath()
	if root.Exists() {
		if root != instance {
			return cue.Path{}, nil
		}
		nested, err := constrainedReference(root.LookupPath(path), instance, depth+1)
		if err != nil || len(nested.Selectors()) > 0 {
			return nested, err
		}
		if typedPath(path) == nil {
			return cue.Path{}, nil
		}
		return path, nil
	}
	if dereferenced := cue.Dereference(value); dereferenced != value {
		return constrainedReference(dereferenced, instance, depth+1)
	}
	if openBoundSliceSource(value) != nil {
		return cue.Path{}, nil
	}
	operation, operands := value.Expr()
	if operation != cue.AndOp {
		return cue.Path{}, nil
	}
	var target cue.Path
	for _, operand := range operands {
		candidate, err := constrainedReference(operand, instance, depth+1)
		if err != nil {
			return cue.Path{}, err
		}
		if len(candidate.Selectors()) > 0 {
			if len(target.Selectors()) > 0 && target.String() != candidate.String() {
				return cue.Path{}, nil
			}
			target = candidate
		} else {
			// Reject reference-bearing transformations even if an independent
			// direct operand also names the same source.
			references, err := expressionReferencePaths(operand, instance, depth+1)
			if err != nil {
				return cue.Path{}, err
			}
			if len(references) > 0 {
				return cue.Path{}, nil
			}
		}
	}
	return target, nil
}

func frozenReferences(instance cue.Value, root cue.Path) ([]frozenReference, error) {
	references := make([]frozenReference, 0)
	ancestors := structuralAncestors(instance.BuildInstance())
	var visit func(cue.Value, cue.Path) error
	visit = func(value cue.Value, path cue.Path) error {
		if value.Err() != nil {
			return errors.New("invalid reference value")
		}
		candidates, err := expressionReferencePaths(value, instance, 0)
		if err != nil {
			return errors.New("reference inspection failed")
		}
		controls, err := structuralReferencePaths(value, instance, ancestors)
		if err != nil {
			return err
		}
		structural := make(map[string]bool)
		for _, control := range controls {
			structural[control.String()] = true
			candidates = append(candidates, control)
		}
		direct, err := constrainedReference(value, instance, 0)
		if err != nil {
			return err
		}
		targets := make(map[string]cue.Path)
		for _, target := range candidates {
			targets[target.String()] = target
		}
		if len(direct.Selectors()) > 0 {
			targets[direct.String()] = direct
		}
		for _, target := range targets {
			primitive := primitiveDeclaration(instance.LookupPath(target))
			references = append(references, frozenReference{Path: typedPath(path), Target: typedPath(target), PrimitiveType: primitive, Supported: primitive != "" && direct.String() == target.String() && !structural[target.String()]})
		}
		return visitChildren(value, path, visit)
	}
	selected := instance.LookupPath(root)
	if !selected.Exists() {
		return references, nil
	}
	if err := visit(selected, root); err != nil {
		return nil, err
	}
	sort.Slice(references, func(i, j int) bool {
		leftPath, _ := references[i].Path.cuePath()
		rightPath, _ := references[j].Path.cuePath()
		if leftPath.String() != rightPath.String() {
			return leftPath.String() < rightPath.String()
		}
		leftTarget, _ := references[i].Target.cuePath()
		rightTarget, _ := references[j].Target.cuePath()
		return leftTarget.String() < rightTarget.String()
	})
	return references, nil
}

func visitChildren(value cue.Value, path cue.Path, visit func(cue.Value, cue.Path) error) error {
	child := func(selector cue.Selector) cue.Path {
		return cue.MakePath(append(append([]cue.Selector{}, path.Selectors()...), selector)...)
	}
	switch value.Kind() {
	case cue.StructKind:
		fields, err := value.Fields(cue.Definitions(false))
		if err != nil {
			return err
		}
		for fields.Next() {
			if err := visit(fields.Value(), child(fields.Selector())); err != nil {
				return err
			}
		}
	case cue.ListKind:
		items, err := value.List()
		if err != nil {
			return err
		}
		for index := 0; items.Next(); index++ {
			if err := visit(items.Value(), child(cue.Index(index))); err != nil {
				return err
			}
		}
	}
	return nil
}

type scalarFill struct {
	Path  valuePath       `json:"path"`
	Value json.RawMessage `json:"value"`
}

type deferredValue struct {
	Path   valuePath `json:"path"`
	Target valuePath `json:"target"`
}

type specializationOptions struct {
	Fills          []scalarFill    `json:"fills"`
	Projection     valuePath       `json:"projection"`
	Deferred       []deferredValue `json:"deferred"`
	ReferencePaths []valuePath     `json:"referencePaths"`
	SourcePaths    *[]valuePath    `json:"sourcePaths"`
	IgnoredPaths   []valuePath     `json:"ignoredPaths"`
	IgnoredGroups  [][]valuePath   `json:"ignoredGroups"`
}

type specializationResult struct {
	Value      interface{}     `json:"value"`
	Unresolved []deferredValue `json:"unresolved"`
}

//export cue_specialize_frozen
func cue_specialize_frozen(snapshot, options *C.char) *C.char {
	return C.CString(specializeFrozenResponse(C.GoString(snapshot), C.GoString(options)))
}

func specializeFrozenResponse(snapshot, optionsJSON string) (result string) {
	defer func() {
		if recover() != nil {
			result = frozenFailure(ErrorCodePanicRecover, "CUE specialization failed")
		}
	}()
	var graph frozenGraph
	var options specializationOptions
	if json.Unmarshal([]byte(snapshot), &graph) != nil || json.Unmarshal([]byte(optionsJSON), &options) != nil {
		return frozenFailure(ErrorCodeInvalidInput, "Invalid CUE specialization options")
	}
	base, err := graph.build()
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "Frozen CUE instance could not be evaluated")
	}
	value, err := specializeValue(base, options)
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "CUE specialization did not satisfy the selected constraints")
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return frozenFailure(ErrorCodeJSONMarshal, "CUE specialization could not be encoded")
	}
	return createSuccessResponse(string(encoded))
}

func specializeValue(base cue.Value, options specializationOptions) (*specializationResult, error) {
	projection, err := options.Projection.cuePath()
	if err != nil || !base.LookupPath(projection).Exists() {
		return nil, errors.New("invalid projection")
	}
	// Inspect provenance before any fill. In particular, source & "literal"
	// is unresolved until source is supplied even though its value is concrete.
	references, err := frozenReferences(base, projection)
	if err != nil {
		return nil, err
	}
	tracked := references
	for _, typed := range options.ReferencePaths {
		path, err := typed.cuePath()
		if err != nil {
			return nil, err
		}
		found, err := frozenReferences(base, path)
		if err != nil {
			return nil, err
		}
		tracked = append(tracked, found...)
	}
	if err := validateUntrackedDependencies(dependencyInspection{instance: base, references: tracked, scope: frozenDependencyScope{SourcePaths: options.SourcePaths, IgnoredPaths: options.IgnoredPaths, IgnoredGroups: options.IgnoredGroups}}); err != nil {
		return nil, err
	}
	return specializeWithReferences(base, options, references)
}

// Only provenance analysis is reusable. Every invocation still checks source
// declarations, applies fresh fills and validates the complete CUE instance.
func specializeWithReferences(base cue.Value, options specializationOptions, references []frozenReference) (*specializationResult, error) {
	projection, err := options.Projection.cuePath()
	if err != nil || !base.LookupPath(projection).Exists() {
		return nil, errors.New("invalid projection")
	}
	allowed := make(map[string]string)
	for _, reference := range references {
		if reference.PrimitiveType == "" {
			continue
		}
		if !reference.Supported {
			return nil, errors.New("unsupported deferred expression")
		}
		path, _ := reference.Path.cuePath()
		target, _ := reference.Target.cuePath()
		allowed[path.String()] = target.String()
	}
	deferred := make(map[string]deferredValue)
	for _, entry := range options.Deferred {
		path, pathErr := entry.Path.cuePath()
		target, targetErr := entry.Target.cuePath()
		key := path.String()
		if pathErr != nil || targetErr != nil || allowed[key] != target.String() || target.String() == "" {
			return nil, errors.New("deferred path does not match a direct reference")
		}
		if _, duplicate := deferred[key]; duplicate {
			return nil, errors.New("duplicate deferred path")
		}
		deferred[key] = entry
	}
	value := base
	filled := make(map[string]bool)
	for _, fill := range options.Fills {
		path, err := fill.Path.cuePath()
		if err != nil || len(path.Selectors()) == 0 || filled[path.String()] || primitiveDeclaration(base.LookupPath(path)) == "" {
			return nil, errors.New("invalid scalar source path")
		}
		var scalar interface{}
		decoder := json.NewDecoder(bytes.NewReader(fill.Value))
		decoder.UseNumber()
		if decoder.Decode(&scalar) != nil {
			return nil, errors.New("invalid scalar value")
		}
		switch scalar.(type) {
		case string, bool, json.Number:
		default:
			return nil, errors.New("source value is not a primitive")
		}
		literal := value.Context().CompileBytes(fill.Value)
		if literal.Validate(cue.Concrete(true), cue.Final()) != nil {
			return nil, errors.New("invalid scalar literal")
		}
		value = value.FillPath(path, literal)
		filled[path.String()] = true
	}
	// Validate the whole instance for conflicts, but require final/concrete
	// values only inside the requested consumer. Unselected abstract values
	// remain legal and can be specialized by another call.
	if value.Validate() != nil {
		return nil, errors.New("source values conflict with CUE constraints")
	}
	unresolved := make([]deferredValue, 0)
	for path, target := range allowed {
		if filled[target] {
			continue
		}
		entry, exists := deferred[path]
		if !exists {
			return nil, errors.New("unresolved source lacks an exact deferred path")
		}
		unresolved = append(unresolved, entry)
	}
	sort.Slice(unresolved, func(i, j int) bool {
		left, _ := unresolved[i].Path.cuePath()
		right, _ := unresolved[j].Path.cuePath()
		return left.String() < right.String()
	})
	unknown := make(map[string]bool)
	for _, entry := range unresolved {
		path, _ := entry.Path.cuePath()
		unknown[path.String()] = true
	}
	selected := value.LookupPath(projection)
	if selected.Validate(cue.Final()) != nil {
		return nil, errors.New("selected value is not final")
	}
	projected, err := projectConcrete(selected, projection, unknown)
	if err != nil {
		return nil, err
	}
	return &specializationResult{Value: projected, Unresolved: unresolved}, nil
}

// Exact deferred leaves export as null, including a narrowed concrete literal.
// Required fields and list lengths are checked separately and cannot be excused
// by marking an ancestor or sibling path as deferred.
func projectConcrete(value cue.Value, path cue.Path, unknown map[string]bool) (interface{}, error) {
	if unknown[path.String()] {
		return nil, nil
	}
	if value.Kind() != cue.StructKind && value.Kind() != cue.ListKind {
		if value.Validate(cue.Concrete(true), cue.Final()) != nil {
			return nil, errors.New("unresolved non-deferred value")
		}
		encoded, err := value.MarshalJSON()
		if err != nil {
			return nil, errors.New("scalar projection failed")
		}
		var result interface{}
		decoder := json.NewDecoder(bytes.NewReader(encoded))
		decoder.UseNumber()
		if decoder.Decode(&result) != nil {
			return nil, errors.New("scalar projection failed")
		}
		return result, nil
	}
	object := make(map[string]interface{})
	items := make([]interface{}, 0)
	if value.Kind() == cue.ListKind && value.Len().Validate(cue.Concrete(true)) != nil {
		return nil, errors.New("list length is not concrete")
	}
	err := visitChildren(value, path, func(child cue.Value, childPath cue.Path) error {
		projected, err := projectConcrete(child, childPath, unknown)
		if err != nil {
			return err
		}
		selector := childPath.Selectors()[len(childPath.Selectors())-1]
		if selector.LabelType() == cue.IndexLabel {
			items = append(items, projected)
		} else {
			object[selector.Unquoted()] = projected
		}
		return nil
	})
	if value.Kind() == cue.ListKind {
		return items, err
	}
	return object, err
}
