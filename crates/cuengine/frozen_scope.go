package main

import (
	"errors"
	"fmt"

	"cuelang.org/go/cue"
)

// Explicit sources include invalid/refined declarations and their descendants.
// Nil discovers bare primitives; an empty list permits bootstrap inspection.
type frozenDependencyScope struct {
	SourcePaths   *[]valuePath
	IgnoredPaths  []valuePath
	IgnoredGroups [][]valuePath
}

type dependencyInspection struct {
	instance   cue.Value
	references []frozenReference
	scope      frozenDependencyScope
}

func pathWithin(path, parent cue.Path) bool {
	selectors, prefix := path.Selectors(), parent.Selectors()
	if len(selectors) < len(prefix) {
		return false
	}
	for index := range prefix {
		if selectors[index] != prefix[index] {
			return false
		}
	}
	return true
}

// A source shared with a visible input cannot authorize an independent check.
// Only pure aliases or the exact expression used by an approved input are safe.
func validateUntrackedDependencies(request dependencyInspection) error {
	instance := request.instance
	var sources []cue.Path
	var ignored [][]cue.Path
	if request.scope.SourcePaths != nil {
		for _, typed := range *request.scope.SourcePaths {
			path, err := typed.cuePath()
			if err != nil || len(path.Selectors()) == 0 || !instance.LookupPath(path).Exists() {
				return errors.New("invalid runtime source declaration path")
			}
			sources = append(sources, path)
			dependencies, err := declaredSourceDependencies(instance.LookupPath(path), instance, 0)
			if err != nil {
				return err
			}
			sources = append(sources, dependencies...)
		}
		if len(sources) == 0 {
			return nil
		}
	}
	for _, typed := range request.scope.IgnoredPaths {
		path, err := typed.cuePath()
		if err != nil || len(path.Selectors()) == 0 {
			return errors.New("invalid ignored subtree path")
		}
		ignored = append(ignored, []cue.Path{path})
	}
	for _, group := range request.scope.IgnoredGroups {
		var paths []cue.Path
		for _, typed := range group {
			path, err := typed.cuePath()
			if err != nil || len(path.Selectors()) == 0 {
				return errors.New("invalid ignored group path")
			}
			paths = append(paths, path)
		}
		ignored = append(ignored, paths)
	}
	relevant := func(consumer, target cue.Path) bool {
		if typedPath(target) == nil {
			return false
		}
		if request.scope.SourcePaths != nil {
			found := false
			for _, source := range sources {
				found = found || pathWithin(target, source)
			}
			if !found {
				return false
			}
		} else if primitiveDeclaration(instance.LookupPath(target)) == "" {
			return false
		}
		for _, group := range ignored {
			consumerInside, sourceInside := false, false
			for _, subtree := range group {
				consumerInside = consumerInside || pathWithin(consumer, subtree)
				sourceInside = sourceInside || pathWithin(target, subtree)
			}
			if consumerInside && sourceInside {
				return false
			}
		}
		return true
	}
	var tracked []cue.Value
	approved := make(map[string]bool)
	for _, reference := range request.references {
		if reference.Supported {
			path, err := reference.Path.cuePath()
			if err != nil {
				return err
			}
			approved[path.String()] = true
			tracked = append(tracked, instance.LookupPath(path))
		}
	}
	ancestors := structuralAncestors(instance.BuildInstance())
	var visit func(cue.Value, cue.Path, int) error
	visit = func(value cue.Value, path cue.Path, depth int) error {
		if depth > 128 {
			return errors.New("global dependency nesting limit exceeded")
		}
		controls, err := inspectStructuralReferences(structuralInspection{value: value, instance: instance, ancestors: ancestors, hiddenControls: true})
		if err != nil {
			return fmt.Errorf("hidden control inspection at %s: %w", path, err)
		}
		visibleControls, err := structuralReferencePaths(value, instance, ancestors)
		if err != nil {
			return fmt.Errorf("visible control inspection at %s: %w", path, err)
		}
		for _, target := range append(controls, visibleControls...) {
			if relevant(path, target) {
				return errors.New("runtime source controls an unsupported global expression")
			}
		}
		candidates, err := expressionReferencePaths(value, instance, 0)
		if err != nil {
			return fmt.Errorf("expression inspection at %s: %w", path, err)
		}
		for _, target := range candidates {
			if !relevant(path, target) || typedPath(path) != nil && approved[path.String()] {
				continue
			}
			direct, err := constrainedReference(value, instance, 0)
			if err != nil {
				return err
			}
			primitive := primitiveDeclaration(value)
			if samePath(direct, target) && primitive != "" && primitive == primitiveDeclaration(instance.LookupPath(target)) {
				continue
			}
			used := false
			for _, consumer := range tracked {
				if (aliasUse{instance: instance, target: path}).contains(consumer, 0) {
					used = true
					break
				}
			}
			if !used {
				return errors.New("runtime source constrains an untracked expression")
			}
		}
		if value.Kind() == cue.StructKind {
			fields, err := value.Fields(cue.Definitions(true), cue.Hidden(true))
			if err != nil {
				return err
			}
			for fields.Next() {
				child := cue.MakePath(append(append([]cue.Selector{}, path.Selectors()...), fields.Selector())...)
				if err := visit(fields.Value(), child, depth+1); err != nil {
					return err
				}
			}
			return nil
		}
		return visitChildren(value, path, func(child cue.Value, childPath cue.Path) error {
			return visit(child, childPath, depth+1)
		})
	}
	return visit(instance, cue.Path{}, 0)
}

type aliasUse struct {
	instance cue.Value
	target   cue.Path
}

func (request aliasUse) contains(value cue.Value, depth int) bool {
	instance, target := request.instance, request.target
	if depth > 32 {
		return false
	}
	root, path := value.ReferencePath()
	if root.Exists() && root == instance {
		return samePath(path, target) || request.contains(root.LookupPath(path), depth+1)
	}
	if dereferenced := cue.Dereference(value); dereferenced != value {
		return request.contains(dereferenced, depth+1)
	}
	if openBoundSliceSource(value) != nil {
		return false
	}
	operation, operands := value.Expr()
	if operation == cue.AndOp {
		for _, operand := range operands {
			if request.contains(operand, depth+1) {
				return true
			}
		}
	}
	return false
}

func samePath(left, right cue.Path) bool {
	return len(left.Selectors()) == len(right.Selectors()) && pathWithin(left, right)
}

// Canonical reference resolution may follow a refined declaration to another
// field. Retain that dependency as a source too; otherwise the explicit source
// filter could hide a predicate that reads an invalid aliased declaration.
func declaredSourceDependencies(value, instance cue.Value, depth int) ([]cue.Path, error) {
	if depth > 32 {
		return nil, errors.New("source declaration nesting limit exceeded")
	}
	dependencies, err := expressionReferencePaths(value, instance, 0)
	if err != nil {
		return nil, err
	}
	err = visitChildren(value, cue.Path{}, func(child cue.Value, _ cue.Path) error {
		nested, err := declaredSourceDependencies(child, instance, depth+1)
		dependencies = append(dependencies, nested...)
		return err
	})
	return dependencies, err
}
