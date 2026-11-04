package main

import (
	"errors"
	"sort"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/provenance"
)

// referenceGraph answers which runtime sources a field depends on, from what the
// evaluator resolved each reference to. Paths are selectors of the analysis build,
// so a reference inside a definition already names the field it was instantiated at.
type referenceGraph struct {
	instance cue.Value
	root     *referenceNode
	// exhausted records that following copies of a struct did not terminate.
	exhausted bool
}

// maxReferencePath bounds a path built by following copies of a struct. CUE rejects
// the structural cycles that could exceed it, so reaching it refuses the analysis.
const maxReferencePath = 512

type referenceNode struct {
	path         cue.Path
	children     map[cue.Selector]*referenceNode
	dependencies []provenance.Dependency
	source       bool
}

func newReferenceGraph(instance cue.Value, sources *[]valuePath) (*referenceGraph, error) {
	dependencies, err := provenance.Analyze(instance)
	if err != nil {
		return nil, err
	}
	graph := &referenceGraph{instance: instance, root: &referenceNode{}}
	for _, dependency := range dependencies {
		field := graph.node(dependency.Field)
		field.dependencies = append(field.dependencies, dependency)
		graph.node(dependency.Target)
	}
	if sources != nil {
		for _, typed := range *sources {
			path, err := typed.cuePath()
			if err != nil || len(path.Selectors()) == 0 {
				return nil, errors.New("invalid runtime source path")
			}
			graph.node(path).source = true
		}
	}
	return graph, nil
}

func (graph *referenceGraph) node(path cue.Path) *referenceNode {
	current := graph.root
	selectors := path.Selectors()
	for index, selector := range selectors {
		child, exists := current.children[selector]
		if !exists {
			child = &referenceNode{path: cue.MakePath(selectors[:index+1]...)}
			if current.children == nil {
				current.children = make(map[cue.Selector]*referenceNode)
			}
			current.children[selector] = child
		}
		current = child
	}
	return current
}

// ancestors returns the nodes above path, with the rest of the path below each.
func (graph *referenceGraph) ancestors(path cue.Path) []referenceAncestor {
	selectors := path.Selectors()
	result := make([]referenceAncestor, 0, len(selectors))
	current := graph.root
	for index, selector := range selectors {
		result = append(result, referenceAncestor{node: current, rest: selectors[index:]})
		current = current.children[selector]
		if current == nil {
			break
		}
	}
	return result
}

// exists reports whether anything is known about path. Without this, a field unified
// with a struct holding it would keep projecting into copies of itself.
func (graph *referenceGraph) exists(path cue.Path) bool {
	current := graph.root
	for _, selector := range path.Selectors() {
		if current = current.children[selector]; current == nil {
			return graph.instance.LookupPath(path).Exists()
		}
	}
	return true
}

type referenceAncestor struct {
	node *referenceNode
	rest []cue.Selector
}

// isSource reports whether a value only becomes known at runtime: a declared source
// or anything below one, and any other bare primitive that refers to nothing, which
// is abstract as far as anything reading it can tell.
func (graph *referenceGraph) isSource(node *referenceNode) bool {
	if node.source {
		return true
	}
	current := graph.root
	for _, selector := range node.path.Selectors() {
		if current.source {
			return true
		}
		current = current.children[selector]
	}
	return len(node.dependencies) == 0 && len(node.path.Selectors()) > 0 && typedPath(node.path) != nil &&
		primitiveDeclaration(graph.instance.LookupPath(node.path)) != ""
}

// isContainer reports whether the fields of path are known without its values. What
// does not exist has no value to depend on.
func (graph *referenceGraph) isContainer(node *referenceNode) bool {
	value := graph.instance.LookupPath(node.path)
	if !value.Exists() {
		return true
	}
	kind := value.IncompleteKind()
	return kind == cue.StructKind || kind == cue.ListKind
}

// strongerUse orders how much a value can be affected: a direct value, a computed
// one, then one whose structure is decided. Presence is settled before this.
func strongerUse(left, right provenance.Use) provenance.Use {
	if left == provenance.Control || right == provenance.Control {
		return provenance.Control
	}
	if left == provenance.Computed || right == provenance.Computed {
		return provenance.Computed
	}
	return provenance.Value
}

type referenceRead struct {
	node *referenceNode
	use  provenance.Use
}

// sourcesOf returns every source the value of start depends on and how. Reading a
// field reads what it refers to, what decides its existence, and when read as a
// whole anything below it. Reading only which fields a struct has reads no values.
func (graph *referenceGraph) sourcesOf(start *referenceNode) map[*referenceNode]provenance.Use {
	found := make(map[*referenceNode]provenance.Use)
	seen := make(map[referenceRead]bool)
	var read func(*referenceNode, provenance.Use)
	follow := func(dependency provenance.Dependency, use provenance.Use) {
		target := graph.node(dependency.Target)
		if dependency.Use == provenance.Presence || use == provenance.Presence {
			read(target, provenance.Presence)
			return
		}
		read(target, strongerUse(use, dependency.Use))
	}
	read = func(node *referenceNode, use provenance.Use) {
		// A scalar is not there yet as far as CUE can tell while its value is unknown,
		// and len counts its contents, so its presence depends on its value too.
		if use == provenance.Presence && !graph.isContainer(node) {
			use = provenance.Control
		}
		key := referenceRead{node: node, use: use}
		if seen[key] {
			return
		}
		seen[key] = true
		if node != start && graph.isSource(node) {
			if previous, exists := found[node]; exists {
				use = strongerUse(previous, use)
			}
			found[node] = use
			return
		}
		for _, dependency := range node.dependencies {
			if use != provenance.Presence || dependency.Use == provenance.Control {
				follow(dependency, use)
			}
		}
		for _, ancestor := range graph.ancestors(node.path) {
			for _, dependency := range ancestor.node.dependencies {
				switch dependency.Use {
				case provenance.Value:
					projected := append(append([]cue.Selector{}, dependency.Target.Selectors()...), ancestor.rest...)
					if len(projected) > maxReferencePath {
						graph.exhausted = true
						continue
					}
					if copied := cue.MakePath(projected...); graph.exists(copied) {
						read(graph.node(copied), use)
					}
				case provenance.Presence:
					read(graph.node(dependency.Target), provenance.Presence)
				default:
					read(graph.node(dependency.Target), provenance.Control)
				}
			}
		}
		switch use {
		case provenance.Value:
			// A struct unified into a field is read field by field.
		case provenance.Presence:
			for _, child := range node.children {
				for _, dependency := range child.dependencies {
					if dependency.Use == provenance.Control {
						follow(dependency, provenance.Control)
					}
				}
			}
		default:
			for _, child := range node.children {
				read(child, use)
			}
		}
	}
	read(start, provenance.Value)
	return found
}

// frozenReferences reports every source each exported value below root depends on.
// A reference is supported unless a source decides the structure around the value.
// A container only reports what its structure depends on.
func (graph *referenceGraph) frozenReferences(root cue.Path) ([]frozenReference, error) {
	paths := make(map[string]cue.Path)
	var collect func(cue.Value, cue.Path) error
	collect = func(value cue.Value, path cue.Path) error {
		paths[path.String()] = path
		switch value.IncompleteKind() {
		case cue.StructKind:
			fields, err := value.Fields(cue.Definitions(false))
			if err != nil {
				// Incomplete structure: what decides it is in the graph, and strict
				// validation refuses whatever is not.
				return nil
			}
			for fields.Next() {
				child := cue.MakePath(append(append([]cue.Selector{}, path.Selectors()...), fields.Selector())...)
				if err := collect(fields.Value(), child); err != nil {
					return err
				}
			}
		case cue.ListKind:
			items, err := value.List()
			if err != nil {
				return nil
			}
			for index := 0; items.Next(); index++ {
				child := cue.MakePath(append(append([]cue.Selector{}, path.Selectors()...), cue.Index(index))...)
				if err := collect(items.Value(), child); err != nil {
					return err
				}
			}
		}
		return nil
	}
	selected := graph.instance.LookupPath(root)
	paths[root.String()] = root
	if selected.Exists() {
		if err := collect(selected, root); err != nil {
			return nil, err
		}
	}
	var pending func(*referenceNode)
	pending = func(node *referenceNode) {
		if typedPath(node.path) != nil {
			paths[node.path.String()] = node.path
		}
		for _, child := range node.children {
			pending(child)
		}
	}
	pending(graph.node(root))

	references := make([]frozenReference, 0)
	for _, path := range paths {
		typed := typedPath(path)
		if typed == nil {
			continue
		}
		node := graph.node(path)
		sources := graph.sourcesOf(node)
		container := graph.instance.LookupPath(path).Exists() && graph.isContainer(node)
		structural := false
		for _, use := range sources {
			structural = structural || use == provenance.Control || (container && use == provenance.Computed)
		}
		for source, use := range sources {
			if container && use == provenance.Value {
				continue
			}
			primitive := primitiveDeclaration(graph.instance.LookupPath(source.path))
			references = append(references, frozenReference{
				Path:          typed,
				Target:        typedPath(source.path),
				PrimitiveType: primitive,
				Supported:     primitive != "" && !structural,
				Computed:      use == provenance.Computed || len(sources) > 1,
			})
		}
	}
	if graph.exhausted {
		return nil, errors.New("reference nesting limit exceeded")
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

// readsSource reports whether the value at path is computed from or carries target,
// and nothing decides its structure.
func (graph *referenceGraph) readsSource(path, target cue.Path) bool {
	reads := false
	for source, use := range graph.sourcesOf(graph.node(path)) {
		if use == provenance.Control {
			return false
		}
		reads = reads || samePath(source.path, target)
	}
	return reads
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

func samePath(left, right cue.Path) bool {
	return len(left.Selectors()) == len(right.Selectors()) && pathWithin(left, right)
}
