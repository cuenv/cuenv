package main

import (
	"errors"
	"fmt"
	"strings"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/token"
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
				if node.Op == token.OR {
					// An arm that is not selected has no path to be visited at.
					if projection := comprehensionControlBody(node); containsControl(projection) {
						if err := scan(projection); err != nil {
							return err
						}
					}
				}
				if err := visit(node.X); err != nil {
					return err
				}
				return visit(node.Y)
			case *ast.ParenExpr:
				return visit(node.X)
			case *ast.UnaryExpr:
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
			case *ast.Ident:
				if binding, ok := node.Node.(*ast.LetClause); ok {
					return visit(binding.Expr)
				}
			case *ast.LetClause:
				return visit(node.Expr)
			case *ast.BasicLit, *ast.BottomLit:
			default:
				// Anything without a rule above is treated as possibly holding a
				// control: only its data field values are excluded from the scan.
				expression, ok := node.(ast.Expr)
				if !ok {
					return fmt.Errorf("structural inspection cannot traverse %T", node)
				}
				projection := comprehensionControlBody(expression)
				if containsControl(projection) {
					if err := scan(projection); err != nil {
						return err
					}
				}
				return visitBoundOperands(projection, visit)
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
	matches := func(candidate cue.Value, depth int) bool { return valueDeclares(candidate, original, depth) }
	if matches(scope, 0) {
		return scope, nil
	}
	selectors := valuePathSelectors(scope)
	for length := len(selectors); length >= 0; length-- {
		candidate := instance.LookupPath(cue.MakePath(selectors[:length]...))
		if candidate.Exists() && matches(candidate, 0) {
			return candidate, nil
		}
	}
	return cue.Value{}, errors.New("original structural binding scope could not be identified")
}

func sourceDeclares(node, original ast.Node) bool {
	if node == original {
		return true
	}
	switch node := node.(type) {
	case *ast.Field:
		return sourceDeclares(node.Value, original)
	case *ast.BinaryExpr:
		return sourceDeclares(node.X, original) || sourceDeclares(node.Y, original)
	case *ast.ParenExpr:
		return sourceDeclares(node.X, original)
	case *ast.UnaryExpr:
		return sourceDeclares(node.X, original)
	case *ast.CallExpr:
		for _, argument := range node.Args {
			if sourceDeclares(argument, original) {
				return true
			}
		}
	case *ast.Ident:
		if binding, ok := node.Node.(*ast.LetClause); ok {
			return sourceDeclares(binding.Expr, original)
		}
		return node.Node == original
	case *ast.StructLit:
		for _, declaration := range node.Elts {
			if embedded, ok := declaration.(*ast.EmbedDecl); ok && sourceDeclares(embedded.Expr, original) {
				return true
			}
		}
	}
	return false
}

// valuePathSelectors is empty for a value with no path. The pinned evaluator's Path
// panics on a value held by a let, whose label has no selector.
func valuePathSelectors(value cue.Value) (selectors []cue.Selector) {
	defer func() {
		if recover() != nil {
			selectors = nil
		}
	}()
	return value.Path().Selectors()
}

// selectionBase returns the operands of a selection, if value is one. The pinned
// evaluator's Expr panics on some values a comprehension produced; those are not.
func selectionBase(value cue.Value) (operands []cue.Value, ok bool) {
	defer func() {
		if recover() != nil {
			operands, ok = nil, false
		}
	}()
	operation, operands := value.Expr()
	return operands, operation == cue.SelectorOp && len(operands) == 2
}

// valueDeclares reports whether original syntax contributes to candidate, using
// only references the evaluator reports. It never compares spelling.
func valueDeclares(candidate cue.Value, original ast.Node, depth int) (declares bool) {
	if depth > 32 {
		return false
	}
	defer func() {
		// Dereference and Expr can panic on values a comprehension produced.
		if recover() != nil {
			declares = false
		}
	}()
	if sourceDeclares(candidate.Source(), original) {
		return true
	}
	if dereferenced := cue.Dereference(candidate); dereferenced != candidate && valueDeclares(dereferenced, original, depth+1) {
		return true
	}
	if openBoundSliceSource(candidate) != nil {
		return false
	}
	operation, operands := candidate.Expr()
	switch operation {
	case cue.AndOp, cue.OrOp:
		for _, operand := range operands {
			if valueDeclares(operand, original, depth+1) {
				return true
			}
		}
	case cue.SelectorOp:
		if len(operands) == 2 {
			if selected, ok := selectLabel(operands[0], operands[1]); ok {
				return valueDeclares(selected, original, depth+1)
			}
		}
	}
	return false
}

// A selector operand is reported by name. Enumerating the base keeps the package
// identity of a hidden definition, which a parsed path would lose.
func selectLabel(base, label cue.Value) (cue.Value, bool) {
	name, err := label.String()
	if err != nil {
		return cue.Value{}, false
	}
	fields, err := base.Fields(cue.Definitions(true), cue.Hidden(true), cue.Optional(true))
	if err != nil {
		return cue.Value{}, false
	}
	for fields.Next() {
		selector := fields.Selector()
		if selector.String() == name || selector.LabelType() == cue.StringLabel && selector.Unquoted() == name {
			return fields.Value(), true
		}
	}
	return cue.Value{}, false
}

// bindingScope finds where an identifier's lexical declaration lives on the path
// being inspected. When the struct itself cannot be matched, the declaring field
// can: the nearest ancestor with a child built from that exact field. A same-named
// field from another conjunct is not that declaration.
func bindingScope(scope, instance cue.Value, identifier *ast.Ident) (cue.Value, error) {
	found, err := originalBindingScope(scope, instance, identifier.Scope)
	if err == nil {
		return found, nil
	}
	// A resolved identifier's Node is the declaring field's value expression.
	declaration, ok := identifier.Node.(ast.Expr)
	if !ok {
		return cue.Value{}, err
	}
	selector := identifierSelector(identifier, instance)
	seen := make(map[string]bool)
	var ascend func(cue.Path, int) (cue.Value, bool)
	ascend = func(path cue.Path, depth int) (cue.Value, bool) {
		if depth > 32 || seen[path.String()] {
			return cue.Value{}, false
		}
		seen[path.String()] = true
		selectors := path.Selectors()
		for length := len(selectors); length >= 0; length-- {
			candidate := instance.LookupPath(cue.MakePath(selectors[:length]...))
			for _, child := range []cue.Value{candidate.LookupPath(cue.MakePath(selector)), candidate.LookupPath(cue.MakePath(selector.Optional()))} {
				if child.Exists() && valueDeclares(child, declaration, 0) {
					return candidate, true
				}
			}
			// _t: #F & {in: x} with wrap: _t.out: the control lives under _t, reached
			// only by following what the evaluator says wrap refers to.
			for _, reference := range containerReferences(candidate, instance, 0) {
				if found, ok := ascend(reference, depth+1); ok {
					return found, true
				}
			}
		}
		return cue.Value{}, false
	}
	if found, ok := ascend(cue.MakePath(valuePathSelectors(scope)...), 0); ok {
		return found, nil
	}
	return cue.Value{}, err
}

// selectedBaseOperands lists the conjuncts of the instantiation a selection reads
// from, at any ancestor of scope. (#F & {in: x}).out has no path to its base, but
// the evaluator reports the base, and x is visible only on its own conjunct.
func selectedBaseOperands(scope, instance cue.Value, declaringScope ast.Node) []cue.Value {
	selectors := valuePathSelectors(scope)
	for length := len(selectors); length >= 0; length-- {
		candidate := instance.LookupPath(cue.MakePath(selectors[:length]...))
		if openBoundSliceSource(candidate) != nil {
			continue
		}
		operands, ok := selectionBase(candidate)
		if !ok || !valueDeclares(operands[0], declaringScope, 0) {
			continue
		}
		if conjunction, conjuncts := operands[0].Expr(); conjunction == cue.AndOp {
			return conjuncts
		}
		return []cue.Value{operands[0]}
	}
	return nil
}

// selectDeclared evaluates a plain reference from the scope declaring its root.
func selectDeclared(scope, instance cue.Value, expression ast.Expr) (cue.Value, bool) {
	switch expression := expression.(type) {
	case *ast.Ident:
		if expression.Scope == nil {
			return cue.Value{}, false
		}
		selected := scope.LookupPath(cue.MakePath(identifierSelector(expression, instance)))
		return selected, selected.Exists()
	case *ast.SelectorExpr:
		base, ok := selectDeclared(scope, instance, expression.X)
		if !ok {
			return cue.Value{}, false
		}
		name, _, err := ast.LabelName(expression.Sel)
		if err != nil {
			return cue.Value{}, false
		}
		selected := base.LookupPath(cue.MakePath(cue.Str(name)))
		return selected, selected.Exists()
	}
	return cue.Value{}, false
}

// withDeclaredLabel rewrites a reference whose root is an alias to the label of
// the field the alias names, so compiling it binds the same field.
func withDeclaredLabel(expression ast.Expr) ast.Expr {
	switch node := expression.(type) {
	case *ast.Ident:
		return declaredIdentifier(node)
	case *ast.SelectorExpr:
		copy := *node
		copy.X = withDeclaredLabel(node.X)
		return &copy
	case *ast.IndexExpr:
		copy := *node
		copy.X = withDeclaredLabel(node.X)
		return &copy
	case *ast.ParenExpr:
		copy := *node
		copy.X = withDeclaredLabel(node.X)
		return &copy
	}
	return expression
}

func declaredIdentifier(identifier *ast.Ident) *ast.Ident {
	structure, ok := identifier.Scope.(*ast.StructLit)
	var declarations []ast.Decl
	if ok {
		declarations = structure.Elts
	} else if file, isFile := identifier.Scope.(*ast.File); isFile {
		declarations = file.Decls
	}
	for _, declaration := range declarations {
		field, ok := declaration.(*ast.Field)
		if !ok || field.Value == nil {
			continue
		}
		alias, isAliased := field.Label.(*ast.Alias)
		_, valueAliased := field.Value.(*ast.Alias)
		if !isAliased && !valueAliased && field.Alias == nil {
			continue
		}
		var value ast.Node = field.Value
		if wrapped, ok := field.Value.(*ast.Alias); ok {
			value = wrapped.Expr
		}
		if identifier.Node != field.Value && identifier.Node != value && identifier.Node != ast.Node(field) {
			continue
		}
		var label ast.Label = field.Label
		if isAliased {
			nested, ok := alias.Expr.(ast.Label)
			if !ok {
				continue
			}
			label = nested
		}
		if name, isIdent, err := ast.LabelName(label); err == nil && isIdent {
			declared := *identifier
			declared.Name = name
			return &declared
		}
	}
	return identifier
}

func identifierSelector(identifier *ast.Ident, instance cue.Value) cue.Selector {
	identifier = declaredIdentifier(identifier)
	switch {
	case strings.HasPrefix(identifier.Name, "_"):
		owner, _ := instanceIdentity(instance, identifier)
		return cue.Hid(identifier.Name, owner)
	case strings.HasPrefix(identifier.Name, "#"):
		return cue.Def(identifier.Name)
	}
	return cue.Str(identifier.Name)
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

// containsControl reports syntax that can decide structure. Dynamic and pattern
// labels count even when the field they label holds only data.
func containsControl(expression ast.Expr) bool {
	found := false
	ast.Walk(expression, func(node ast.Node) bool {
		switch node := node.(type) {
		case *ast.Comprehension, *ast.IndexExpr, *ast.SliceExpr:
			found = true
		case *ast.Field:
			found = found || dynamicLabel(node.Label)
		}
		return !found
	}, nil)
	return found
}

func dynamicLabel(label ast.Label) bool {
	if alias, ok := label.(*ast.Alias); ok {
		if nested, ok := alias.Expr.(ast.Label); ok {
			return dynamicLabel(nested)
		}
		return true
	}
	_, _, err := ast.LabelName(label)
	return err != nil
}

// Operands of a call or other expression can name a let that holds the control.
func visitBoundOperands(expression ast.Expr, visit func(ast.Node) error) error {
	var failure error
	ast.Walk(expression, func(node ast.Node) bool {
		if identifier, ok := node.(*ast.Ident); ok && failure == nil {
			if binding, ok := identifier.Node.(*ast.LetClause); ok {
				failure = visit(binding.Expr)
			}
		}
		return failure == nil
	}, nil)
	return failure
}
