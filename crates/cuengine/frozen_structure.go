package main

import (
	"errors"
	"strings"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/build"
)

// Evaluated field iteration and even Syntax(Raw) can erase the expression which
// created a concrete label or selected a concrete index. Inspect original source
// nodes as well, in their CUE scopes. Only structural controls are inspected here:
// ordinary nested fields are visited at their own paths by frozenReferences.
func structuralReferencePaths(value, instance cue.Value, ancestors map[ast.Node][]ast.Expr) ([]cue.Path, error) {
	return inspectStructuralReferences(structuralInspection{value: value, instance: instance, ancestors: ancestors})
}

type structuralInspection struct {
	value, instance cue.Value
	ancestors       map[ast.Node][]ast.Expr
	hiddenControls  bool
}

func inspectStructuralReferences(request structuralInspection) ([]cue.Path, error) {
	value, instance, ancestors := request.value, request.instance, request.ancestors
	var references []cue.Path
	seen := make(map[ast.Node]bool)
	inspected := make(map[cue.Value]bool)
	var inspect func(cue.Value, int) error
	inspect = func(scope cue.Value, depth int) error {
		if inspected[scope] {
			return nil
		}
		inspected[scope] = true
		if depth > 128 {
			return errors.New("structural reference nesting limit exceeded")
		}
		scan := func(expression ast.Expr) error {
			found, err := inspectRawReferencePaths(rawReferenceInspection{value: value, instance: instance, expression: expression, originalSource: true})
			if err != nil {
				return err
			}
			references = append(references, found...)
			return nil
		}
		var visit func(ast.Node) error
		visit = func(node ast.Node) error {
			if node == nil || seen[node] {
				return nil
			}
			seen[node] = true
			switch node := node.(type) {
			case *ast.File:
				return visit(&ast.StructLit{Elts: node.Decls})
			case *ast.StructLit:
				for _, declaration := range node.Elts {
					switch declaration := declaration.(type) {
					case *ast.Field:
						if request.hiddenControls {
							continue
						}
						if _, _, err := ast.LabelName(declaration.Label); err != nil {
							if expression, ok := declaration.Label.(ast.Expr); ok {
								if err := scan(expression); err != nil {
									return err
								}
							}
						}
					case *ast.Comprehension:
						if request.hiddenControls {
							control := *declaration
							control.Value = comprehensionControlBody(control.Value)
							if control.Fallback != nil {
								fallback := *control.Fallback
								fallback.Body = comprehensionControlBody(fallback.Body).(*ast.StructLit)
								control.Fallback = &fallback
							}
							if err := scan(&control); err != nil {
								return err
							}
							continue
						}
						if !comprehensionEmitsVisibleStructure(declaration) {
							continue
						}
						if err := scan(declaration); err != nil {
							return err
						}
					case *ast.EmbedDecl:
						if err := visit(declaration.Expr); err != nil {
							return err
						}
					}
				}
			case *ast.Field:
				return visit(node.Value)
			case *ast.Comprehension:
				if request.hiddenControls {
					return visit(&ast.StructLit{Elts: []ast.Decl{node}})
				}
				if !comprehensionEmitsVisibleStructure(node) {
					return nil
				}
				return scan(node)
			case *ast.ListLit:
				if request.hiddenControls {
					return nil
				}
				for _, element := range node.Elts {
					if comprehension, ok := element.(*ast.Comprehension); ok {
						if err := scan(comprehension); err != nil {
							return err
						}
					}
				}
			case *ast.BinaryExpr:
				if err := visit(node.X); err != nil {
					return err
				}
				return visit(node.Y)
			case *ast.ParenExpr:
				return visit(node.X)
			case *ast.Alias:
				return visit(node.Expr)
			case *ast.IndexExpr:
				if request.hiddenControls {
					return nil
				}
				if err := scan(node.Index); err != nil {
					return err
				}
				return visit(node.X)
			case *ast.SelectorExpr:
				return visit(node.X)
			case *ast.SliceExpr:
				if request.hiddenControls {
					return nil
				}
				return scan(node)
			}
			return nil
		}
		if controls := ancestors[scope.Source()]; len(controls) > 0 {
			// CUE may expose an inner control as the source of an emitted
			// field. Start at its outermost control to retain every binding.
			if request.hiddenControls {
				if err := visit(controls[0]); err != nil {
					return err
				}
			} else if err := scan(controls[0]); err != nil {
				return err
			}
		} else if err := visit(scope.Source()); err != nil {
			return err
		}
		root, path := scope.ReferencePath()
		if root.Exists() && root == instance {
			return inspect(root.LookupPath(path), depth+1)
		}
		if dereferenced := cue.Dereference(scope); dereferenced != scope {
			if err := inspect(dereferenced, depth+1); err != nil {
				return err
			}
		}
		if openBoundSliceSource(scope) != nil {
			return nil
		}
		operation, operands := scope.Expr()
		if operation == cue.AndOp {
			for _, operand := range operands {
				if err := inspect(operand, depth+1); err != nil {
					return err
				}
			}
		}
		return nil
	}
	if err := inspect(value, 0); err != nil {
		return nil, err
	}
	return references, nil
}

func structuralAncestors(root *build.Instance) map[ast.Node][]ast.Expr {
	result := make(map[ast.Node][]ast.Expr)
	seen := make(map[*build.Instance]bool)
	var visit func(*build.Instance)
	visit = func(instance *build.Instance) {
		if instance == nil || seen[instance] {
			return
		}
		seen[instance] = true
		for _, file := range instance.Files {
			var controls []ast.Expr
			ast.Walk(file, func(node ast.Node) bool {
				if len(controls) > 0 {
					result[node] = append([]ast.Expr{}, controls...)
				}
				if comprehension, ok := node.(*ast.Comprehension); ok {
					controls = append(controls, comprehension)
				}
				return true
			}, func(node ast.Node) {
				if _, ok := node.(*ast.Comprehension); ok {
					controls = controls[:len(controls)-1]
				}
			})
		}
		for _, imported := range instance.Imports {
			visit(imported)
		}
	}
	visit(root)
	return result
}

// Rebinding original syntax must retain its original lexical scope, even when
// a consumer shadows the name or a let binding was declared in an ancestor.
// Source-node identity locates that scope; CUE still performs name resolution.
func originalBindingScope(scope, instance cue.Value, original ast.Node) (cue.Value, error) {
	if file, ok := original.(*ast.File); ok {
		seen := make(map[*build.Instance]bool)
		var find func(*build.Instance) *build.Instance
		find = func(candidate *build.Instance) *build.Instance {
			if candidate == nil || seen[candidate] {
				return nil
			}
			seen[candidate] = true
			for _, source := range candidate.Files {
				if source == file {
					return candidate
				}
			}
			for _, imported := range candidate.Imports {
				if found := find(imported); found != nil {
					return found
				}
			}
			return nil
		}
		if found := find(instance.BuildInstance()); found != nil {
			if found == instance.BuildInstance() {
				return instance, nil
			}
			return instance.Context().BuildInstance(found), nil
		}
	}
	var sourceMatches func(ast.Node) bool
	sourceMatches = func(node ast.Node) bool {
		if node == original {
			return true
		}
		switch node := node.(type) {
		case *ast.Field:
			return sourceMatches(node.Value)
		case *ast.BinaryExpr:
			return sourceMatches(node.X) || sourceMatches(node.Y)
		case *ast.ParenExpr:
			return sourceMatches(node.X)
		}
		return false
	}
	var matches func(cue.Value, int) bool
	matches = func(candidate cue.Value, depth int) bool {
		if depth > 32 {
			return false
		}
		if sourceMatches(candidate.Source()) {
			return true
		}
		if dereferenced := cue.Dereference(candidate); dereferenced != candidate && matches(dereferenced, depth+1) {
			return true
		}
		if openBoundSliceSource(candidate) != nil {
			return false
		}
		operation, operands := candidate.Expr()
		if operation == cue.AndOp {
			for _, operand := range operands {
				if matches(operand, depth+1) {
					return true
				}
			}
		}
		return false
	}
	if matches(scope, 0) {
		return scope, nil
	}
	selectors := scope.Path().Selectors()
	for length := len(selectors); length >= 0; length-- {
		candidate := instance.LookupPath(cue.MakePath(selectors[:length]...))
		if candidate.Exists() && matches(candidate, 0) {
			return candidate, nil
		}
	}
	return cue.Value{}, errors.New("original structural binding scope could not be identified")
}

// Hidden validation fields do not change exported input structure. Their native
// CUE constraints still run during BuildInstance/Validate and after every fill.
// Do not try to reinterpret schema-owned name/presence checks as dynamic inputs.
func comprehensionEmitsVisibleStructure(comprehension *ast.Comprehension) bool {
	return emitsVisibleStructure(comprehension.Value) ||
		comprehension.Fallback != nil && emitsVisibleStructure(comprehension.Fallback.Body)
}

func emitsVisibleStructure(expression ast.Expr) bool {
	structure, ok := expression.(*ast.StructLit)
	if !ok {
		return true
	}
	for _, declaration := range structure.Elts {
		switch declaration := declaration.(type) {
		case *ast.Field:
			name, identifier, err := ast.LabelName(declaration.Label)
			if err != nil || !identifier || (!strings.HasPrefix(name, "_") && !strings.HasPrefix(name, "#")) {
				return true
			}
		case *ast.Comprehension:
			if comprehensionEmitsVisibleStructure(declaration) {
				return true
			}
		case *ast.LetClause, *ast.Attribute:
		default:
			return true
		}
	}
	return false
}
