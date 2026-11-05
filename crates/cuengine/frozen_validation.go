package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"errors"

	"cuelang.org/go/cue"
	cueerrors "cuelang.org/go/cue/errors"
)

type frozenValidationOptions struct {
	ConcretePaths []valuePath     `json:"concretePaths"`
	AbstractPaths []valuePath     `json:"abstractPaths"`
	Deferred      []deferredValue `json:"deferred"`
	SourcePaths   *[]valuePath    `json:"sourcePaths"`
}

//export cue_validate_frozen
func cue_validate_frozen(snapshot, options *C.char) *C.char {
	return C.CString(validateFrozenResponse(C.GoString(snapshot), C.GoString(options)))
}

func validateFrozenResponse(snapshot, optionsJSON string) (result string) {
	defer func() {
		if recover() != nil {
			result = frozenFailure(ErrorCodePanicRecover, "Frozen CUE validation failed")
		}
	}()
	var graph frozenGraph
	var options frozenValidationOptions
	if json.Unmarshal([]byte(snapshot), &graph) != nil || json.Unmarshal([]byte(optionsJSON), &options) != nil {
		return frozenFailure(ErrorCodeInvalidInput, "Invalid frozen CUE validation options")
	}
	instance, err := graph.build()
	if err == nil {
		err = validateFrozenInstance(graph, instance, options)
	}
	if err != nil {
		return frozenFailure(analysisFailureCode(err), "Frozen CUE instance did not satisfy the selected constraints")
	}
	return createSuccessResponse("true")
}

// validateFrozenInstance is the strict discovery gate. Each selected subtree
// must be final and concrete except exact verified references and explicitly
// selected namespaces of bare primitive declarations. No disk reload occurs.
func validateFrozenInstance(frozen frozenGraph, instance cue.Value, options frozenValidationOptions) error {
	abstract := make(map[string]bool)
	for _, typed := range options.AbstractPaths {
		path, err := typed.cuePath()
		if err != nil || len(path.Selectors()) == 0 {
			return errors.New("invalid abstract namespace path")
		}
		value := instance.LookupPath(path)
		if !value.Exists() || validateAbstractNamespace(value, path) != nil {
			return errors.New("invalid abstract namespace")
		}
		abstract[path.String()] = true
	}
	roots := make([]cue.Path, 0, len(options.ConcretePaths)+len(options.Deferred))
	for _, typed := range options.ConcretePaths {
		path, err := typed.cuePath()
		if err != nil {
			return err
		}
		roots = append(roots, path)
	}
	for _, entry := range options.Deferred {
		path, err := entry.Path.cuePath()
		if err != nil {
			return err
		}
		roots = append(roots, path)
	}
	graph, err := frozen.references(instance, options.SourcePaths, roots...)
	if err != nil {
		return err
	}
	deferred := make(map[string]bool)
	seen := make(map[string]bool)
	for _, entry := range options.Deferred {
		path, err := entry.Path.cuePath()
		if err != nil || len(path.Selectors()) == 0 || !instance.LookupPath(path).Exists() {
			return errors.New("invalid deferred path")
		}
		target, err := entry.Target.cuePath()
		if err != nil || len(target.Selectors()) == 0 || primitiveDeclaration(instance.LookupPath(target)) == "" {
			return errors.New("invalid deferred source")
		}
		if !graph.readsSource(path, target) {
			return errors.New("deferred path does not match a reference")
		}
		pair := path.String() + "<-" + target.String()
		if seen[pair] {
			return errors.New("duplicate deferred reference")
		}
		seen[pair] = true
		deferred[path.String()] = true
	}
	var visit func(cue.Value, cue.Path) error
	visit = func(value cue.Value, path cue.Path) error {
		if abstract[path.String()] || deferred[path.String()] {
			return nil
		}
		switch value.Kind() {
		case cue.StructKind:
			return visitChildren(value, path, visit)
		case cue.ListKind:
			if value.Len().Validate(cue.Concrete(true)) != nil {
				return errors.New("list length is not concrete")
			}
			return visitChildren(value, path, visit)
		default:
			if value.Validate(cue.Concrete(true), cue.Final()) != nil {
				return errors.New("non-deferred value is not concrete")
			}
			return nil
		}
	}
	for _, typed := range options.ConcretePaths {
		path, err := typed.cuePath()
		if err != nil {
			return err
		}
		value := instance.LookupPath(path)
		if !value.Exists() || validateFinal(instance, value, deferred) != nil {
			return errors.New("selected subtree is not final")
		}
		references, err := graph.frozenReferences(path)
		if err != nil {
			return err
		}
		for _, reference := range references {
			if reference.PrimitiveType == "" {
				continue
			}
			consumer, _ := reference.Path.cuePath()
			if !reference.Supported || !deferred[consumer.String()] {
				return errors.New("abstract source lacks an exact supported deferred leaf")
			}
		}
		if err := visit(value, path); err != nil {
			return err
		}
	}
	return nil
}

// validateFinal checks everything in value, part of instance, is final except what
// is computed from a value that is not known yet: the expression producing it cannot
// complete before then. It may still be missing a value, but nothing else may be wrong.
func validateFinal(instance, value cue.Value, unknown map[string]bool) error {
	err := value.Validate(cue.Final())
	if err == nil {
		return nil
	}
	for _, failure := range cueerrors.Errors(err) {
		path := failure.Path()
		selectors := make([]cue.Selector, 0, len(path))
		for _, label := range path {
			selectors = append(selectors, cue.ParsePath(label).Selectors()...)
		}
		location := cue.MakePath(selectors...)
		if !unknown[location.String()] || instance.LookupPath(location).Validate() != nil {
			return err
		}
	}
	return nil
}
