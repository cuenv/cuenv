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
	analysis, err := graph.buildAnalysis()
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "Frozen CUE instance could not be evaluated")
	}
	base, err := graph.build()
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "Frozen CUE instance could not be evaluated")
	}
	value, err := specializeValue(analysis, base, options)
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "CUE specialization did not satisfy the selected constraints")
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return frozenFailure(ErrorCodeJSONMarshal, "CUE specialization could not be encoded")
	}
	return createSuccessResponse(string(encoded))
}

// Provenance comes from the analysis build; the fills and validation producing
// the projected value run on a normally evaluated base.
func specializeValue(analysis, base cue.Value, options specializationOptions) (*specializationResult, error) {
	projection, err := options.Projection.cuePath()
	if err != nil || !analysis.LookupPath(projection).Exists() {
		return nil, errors.New("invalid projection")
	}
	// Inspect provenance before any fill. In particular, source & "literal"
	// is unresolved until source is supplied even though its value is concrete.
	graph, err := newReferenceGraph(analysis, options.SourcePaths)
	if err != nil {
		return nil, err
	}
	references, err := graph.frozenReferences(projection)
	if err != nil {
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
	// A computed value reads several sources: it is unknown until all of them are known.
	allowed := make(map[string]map[string]cue.Path)
	for _, reference := range references {
		if reference.PrimitiveType == "" {
			continue
		}
		if !reference.Supported {
			return nil, errors.New("unsupported deferred expression")
		}
		path, _ := reference.Path.cuePath()
		target, _ := reference.Target.cuePath()
		if allowed[path.String()] == nil {
			allowed[path.String()] = make(map[string]cue.Path)
		}
		allowed[path.String()][target.String()] = target
	}
	deferred := make(map[string]deferredValue)
	for _, entry := range options.Deferred {
		path, pathErr := entry.Path.cuePath()
		target, targetErr := entry.Target.cuePath()
		if pathErr != nil || targetErr != nil || target.String() == "" {
			return nil, errors.New("deferred path does not match a reference")
		}
		if _, declared := allowed[path.String()][target.String()]; !declared {
			return nil, errors.New("deferred path does not match a reference")
		}
		pair := path.String() + "<-" + target.String()
		if _, duplicate := deferred[pair]; duplicate {
			return nil, errors.New("duplicate deferred reference")
		}
		deferred[pair] = entry
	}
	value := base
	filled := make(map[string]bool)
	for _, fill := range options.Fills {
		path, err := fill.Path.cuePath()
		if err != nil || len(path.Selectors()) == 0 || filled[path.String()] || primitiveDeclaration(base.LookupPath(path)) == "" {
			return nil, errors.New("invalid scalar source path")
		}
		expression, err := scalarExpression(fill.Value)
		if err != nil {
			return nil, err
		}
		literal := value.Context().BuildExpr(expression)
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
	unknown := make(map[string]bool)
	for path, targets := range allowed {
		for target := range targets {
			if filled[target] {
				continue
			}
			entry, exists := deferred[path+"<-"+target]
			if !exists {
				return nil, errors.New("unresolved source lacks an exact deferred path")
			}
			unresolved = append(unresolved, entry)
			unknown[path] = true
		}
	}
	sort.Slice(unresolved, func(i, j int) bool {
		left, _ := unresolved[i].Path.cuePath()
		right, _ := unresolved[j].Path.cuePath()
		if left.String() != right.String() {
			return left.String() < right.String()
		}
		leftTarget, _ := unresolved[i].Target.cuePath()
		rightTarget, _ := unresolved[j].Target.cuePath()
		return leftTarget.String() < rightTarget.String()
	})
	selected := value.LookupPath(projection)
	if validateFinal(selected, unknown) != nil {
		return nil, errors.New("selected value is not final")
	}
	projected, err := projectConcrete(selected, projection, unknown)
	if err != nil {
		return nil, err
	}
	return &specializationResult{Value: projected, Unresolved: unresolved}, nil
}

// Exact deferred leaves export as null, including a narrowed concrete literal and a
// value computed from any source that is not known yet.
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
