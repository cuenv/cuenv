package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"errors"

	"cuelang.org/go/cue"
)

type frozenValidationOptions struct {
	ConcretePaths []valuePath     `json:"concretePaths"`
	AbstractPaths []valuePath     `json:"abstractPaths"`
	Deferred      []deferredValue `json:"deferred"`
	SourcePaths   *[]valuePath    `json:"sourcePaths"`
	IgnoredPaths  []valuePath     `json:"ignoredPaths"`
	IgnoredGroups [][]valuePath   `json:"ignoredGroups"`
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
	if err != nil || validateFrozenInstance(instance, options) != nil {
		return frozenFailure(ErrorCodeBuildValue, "Frozen CUE instance did not satisfy the selected constraints")
	}
	return createSuccessResponse("true")
}

// validateFrozenInstance is the strict discovery gate. Each selected subtree
// must be final and concrete except exact verified references and explicitly
// selected namespaces of bare primitive declarations. No disk reload occurs.
func validateFrozenInstance(instance cue.Value, options frozenValidationOptions) error {
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
	deferred := make(map[string]bool)
	for _, entry := range options.Deferred {
		path, err := entry.Path.cuePath()
		if err != nil || len(path.Selectors()) == 0 || !instance.LookupPath(path).Exists() {
			return errors.New("invalid deferred path")
		}
		target, err := entry.Target.cuePath()
		if err != nil || len(target.Selectors()) == 0 || primitiveDeclaration(instance.LookupPath(target)) == "" {
			return errors.New("invalid deferred source")
		}
		actual, err := constrainedReference(instance.LookupPath(path), instance, 0)
		if err != nil || actual.String() != target.String() {
			return errors.New("deferred path does not match a direct reference")
		}
		if deferred[path.String()] {
			return errors.New("duplicate deferred path")
		}
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
	var tracked []frozenReference
	for _, typed := range options.ConcretePaths {
		path, err := typed.cuePath()
		if err != nil {
			return err
		}
		value := instance.LookupPath(path)
		if !value.Exists() || value.Validate(cue.Final()) != nil {
			return errors.New("selected subtree is not final")
		}
		references, err := frozenReferences(instance, path)
		if err != nil {
			return err
		}
		tracked = append(tracked, references...)
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
	return validateUntrackedDependencies(dependencyInspection{instance: instance, references: tracked, scope: frozenDependencyScope{SourcePaths: options.SourcePaths, IgnoredPaths: options.IgnoredPaths, IgnoredGroups: options.IgnoredGroups}})
}
