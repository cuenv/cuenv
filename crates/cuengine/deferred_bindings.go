package main

import (
	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"fmt"
)

// These are original CUE values from each actual iteration, not reconstructed
// JSON. Selecting a bound field therefore retains its real source identity.
type referenceBindingName struct {
	name        string
	declaration ast.Node
}

func bindingName(identifier *ast.Ident) referenceBindingName {
	return referenceBindingName{identifier.Name, identifier.Node}
}

type referenceBindings map[referenceBindingName][]cue.Value

func plainReference(expression ast.Expr) bool {
	switch expression := expression.(type) {
	case *ast.Ident:
		return true
	case *ast.SelectorExpr:
		return plainReference(expression.X)
	case *ast.IndexExpr:
		return plainReference(expression.X)
	case *ast.ParenExpr:
		return plainReference(expression.X)
	}
	return false
}

func (bindings referenceBindings) selectValues(expression ast.Expr, resolve func(ast.Expr) cue.Value) ([]cue.Value, bool) {
	switch expression := expression.(type) {
	case *ast.Ident:
		values, exists := bindings[bindingName(expression)]
		return values, exists
	case *ast.SelectorExpr:
		values, bound := bindings.selectValues(expression.X, resolve)
		if !bound {
			return nil, false
		}
		name, _, err := ast.LabelName(expression.Sel)
		if err != nil {
			return nil, false
		}
		var selected []cue.Value
		for _, value := range values {
			child := value.LookupPath(cue.MakePath(cue.Str(name)))
			if child.Exists() {
				selected = append(selected, child)
			}
		}
		return selected, true
	case *ast.IndexExpr:
		values, bound := bindings.selectValues(expression.X, resolve)
		if !bound {
			return nil, false
		}
		indices, indexed := bindings.selectValues(expression.Index, resolve)
		if !indexed {
			indices = []cue.Value{resolve(expression.Index)}
		}
		var selected []cue.Value
		for _, index := range indices {
			var selector cue.Selector
			if field, err := index.String(); err == nil {
				selector = cue.Str(field)
			} else if position, err := index.Int64(); err == nil && position >= 0 && int64(int(position)) == position {
				selector = cue.Index(int(position))
			} else {
				return nil, false
			}
			for _, value := range values {
				child := value.LookupPath(cue.MakePath(selector))
				if child.Exists() {
					selected = append(selected, child)
				}
			}
		}
		return selected, true
	case *ast.ParenExpr:
		return bindings.selectValues(expression.X, resolve)
	}
	return nil, false
}

func originalReferenceValue(value, instance cue.Value) cue.Value {
	root, path := value.ReferencePath()
	if root.Exists() && root == instance {
		return root.LookupPath(path)
	}
	return value
}

func iterationBindings(source, instance cue.Value) (keys, values []cue.Value) {
	source = originalReferenceValue(source, instance)
	switch source.Kind() {
	case cue.StructKind:
		fields, err := source.Fields()
		if err == nil {
			for fields.Next() {
				keys = append(keys, source.Context().Encode(fields.Selector().Unquoted()))
				values = append(values, originalReferenceValue(fields.Value(), instance))
			}
		}
	case cue.ListKind:
		items, err := source.List()
		if err == nil {
			for index := 0; items.Next(); index++ {
				keys = append(keys, source.Context().Encode(index))
				values = append(values, originalReferenceValue(items.Value(), instance))
			}
		}
	}
	return keys, values
}

// Keep every nested control and its lexical lets. Dropping a comprehension's
// whole body would erase a false nested condition before it can be inspected.
func comprehensionControlBody(expression ast.Expr) ast.Expr {
	structure, ok := expression.(*ast.StructLit)
	if !ok {
		switch expression := expression.(type) {
		case *ast.ParenExpr:
			copy := *expression
			copy.X = comprehensionControlBody(copy.X)
			return &copy
		case *ast.BinaryExpr:
			copy := *expression
			copy.X, copy.Y = comprehensionControlBody(copy.X), comprehensionControlBody(copy.Y)
			return &copy
		case *ast.Comprehension:
			copy := *expression
			copy.Value = comprehensionControlBody(copy.Value)
			if copy.Fallback != nil {
				fallback := *copy.Fallback
				fallback.Body = comprehensionControlBody(fallback.Body).(*ast.StructLit)
				copy.Fallback = &fallback
			}
			return &copy
		case *ast.Func:
			copy := *expression
			copy.Args = append([]ast.Expr(nil), expression.Args...)
			for index, argument := range copy.Args {
				copy.Args[index] = comprehensionControlBody(argument)
			}
			copy.Ret = comprehensionControlBody(copy.Ret)
			return &copy

		case *ast.ListLit:
			copy := *expression
			copy.Elts = append([]ast.Expr(nil), expression.Elts...)
			for index, element := range copy.Elts {
				copy.Elts[index] = comprehensionControlBody(element)
			}
			return &copy
		case *ast.CallExpr:
			copy := *expression
			copy.Fun = comprehensionControlBody(copy.Fun)
			copy.Args = append([]ast.Expr(nil), expression.Args...)
			for index, argument := range copy.Args {
				copy.Args[index] = comprehensionControlBody(argument)
			}
			return &copy
		case *ast.UnaryExpr:
			copy := *expression
			copy.X = comprehensionControlBody(copy.X)
			return &copy
		case *ast.SelectorExpr:
			copy := *expression
			copy.X = comprehensionControlBody(copy.X)
			return &copy
		case *ast.IndexExpr:
			copy := *expression
			copy.X, copy.Index = comprehensionControlBody(copy.X), comprehensionControlBody(copy.Index)
			return &copy
		case *ast.SliceExpr:
			copy := *expression
			copy.X = comprehensionControlBody(copy.X)
			copy.Low, copy.High = comprehensionControlBody(copy.Low), comprehensionControlBody(copy.High)
			return &copy
		case *ast.Interpolation:
			copy := *expression
			copy.Elts = append([]ast.Expr(nil), expression.Elts...)
			for index, element := range copy.Elts {
				copy.Elts[index] = comprehensionControlBody(element)
			}
			return &copy
		case *ast.Ellipsis:
			copy := *expression
			copy.Type = comprehensionControlBody(copy.Type)
			return &copy

		case *ast.Alias:
			copy := *expression
			copy.Expr = comprehensionControlBody(copy.Expr)
			return &copy
		}
		return expression
	}
	var declarations []ast.Decl
	for _, declaration := range structure.Elts {
		switch declaration := declaration.(type) {
		case *ast.Comprehension:
			control := *declaration
			control.Value = comprehensionControlBody(control.Value)
			if control.Fallback != nil {
				fallback := *control.Fallback
				fallback.Body = comprehensionControlBody(fallback.Body).(*ast.StructLit)
				control.Fallback = &fallback
			}
			declarations = append(declarations, &control)
		case *ast.LetClause:
			declarations = append(declarations, declaration)
		case *ast.EmbedDecl:
			embedded := *declaration
			embedded.Expr = comprehensionControlBody(embedded.Expr)
			declarations = append(declarations, &embedded)
		case *ast.Field:
			// Parentheses, conjunctions and other expression nodes do not
			// remove the controls nested inside a field's value. Inspect the
			// original syntax rather than requiring a direct struct literal.
			if hasControlStructure(declaration.Value) {
				field := *declaration
				field.Value = comprehensionControlBody(field.Value)
				declarations = append(declarations, &field)
			}
		}
	}
	retained := &ast.StructLit{Elts: declarations}
	needed := referencedLexicalLets(retained)
	retained.Elts = nil
	for _, declaration := range declarations {
		if lexical, ok := declaration.(*ast.LetClause); ok && !needed[lexical] {
			continue
		}
		retained.Elts = append(retained.Elts, declaration)
	}
	return retained
}

// hasControlStructure traverses syntax, not evaluated values: a false hidden
// branch may disappear from CUE's exported value before its dependency is seen.
func hasControlStructure(expression ast.Expr) bool {
	found := false
	ast.Walk(expression, func(node ast.Node) bool {
		if _, ok := node.(*ast.StructLit); ok {
			found = true
		}
		return !found
	}, nil)
	return found
}

// Expr operands can lose their evaluated iteration scope. Inspect the selected
// original operand with its enclosing clauses, without attributing expressions
// from sibling fields to it. Lexical lets in each control body remain available.
func expressionWithIterationBindings(expression ast.Expr, controls []ast.Expr) (ast.Expr, error) {
	var target ast.Node = expression
	for index := len(controls) - 1; index >= 0; index-- {
		control, ok := controls[index].(*ast.Comprehension)
		if !ok {
			continue
		}
		body, found := expressionWithLexicalLets(control.Value, target, expression)
		if !found && control.Fallback != nil {
			body, found = expressionWithLexicalLets(control.Fallback.Body, target, expression)
		}
		if !found {
			return nil, fmt.Errorf("original iteration operand scope could not be identified")
		}
		copy := *control
		copy.Value = &ast.StructLit{Elts: []ast.Decl{&ast.EmbedDecl{Expr: body}}}
		copy.Fallback = nil
		expression = &copy
		target = control
	}
	return expression, nil
}

// Retain lexical lets from every struct between a control and the selected
// original operand, including structs nested under expression wrappers. Keep
// those scopes nested, and omit unrelated field values and sibling controls.
func expressionWithLexicalLets(root, target ast.Node, expression ast.Expr) (ast.Expr, bool) {
	var stack []*ast.StructLit
	var scopes []*ast.StructLit
	found := false
	ast.Walk(root, func(node ast.Node) bool {
		if found {
			return false
		}
		if node == target {
			scopes = append([]*ast.StructLit(nil), stack...)
			found = true
			return false
		}
		if structure, ok := node.(*ast.StructLit); ok {
			stack = append(stack, structure)
		}
		return true
	}, func(node ast.Node) {
		if !found {
			if _, ok := node.(*ast.StructLit); ok {
				stack = stack[:len(stack)-1]
			}
		}
	})
	needed := referencedLexicalLets(expression)
	for index := len(scopes) - 1; index >= 0; index-- {
		var declarations []ast.Decl
		for _, declaration := range scopes[index].Elts {
			if lexical, ok := declaration.(*ast.LetClause); ok && needed[lexical] {
				declarations = append(declarations, declaration)
			}
		}
		if len(declarations) > 0 {
			declarations = append(declarations, &ast.EmbedDecl{Expr: expression})
			expression = &ast.StructLit{Elts: declarations}
		}
	}
	return expression, found
}

// Literal iteration lists have fixed membership. Their elements may be bound
// containers, but computed or constrained elements still need normal checking.
func plainIterationSource(expression ast.Expr) bool {
	if plainReference(expression) {
		return true
	}
	switch expression := expression.(type) {
	case *ast.ParenExpr:
		return plainIterationSource(expression.X)
	case *ast.ListLit:
		for _, element := range expression.Elts {
			if !plainReference(element) {
				return false
			}
		}
		return true
	}
	return false
}

// Keep the original CUE values of literal-list elements. Rebuilding a list from
// JSON or evaluating a bare iteration variable would erase its source identity.
func (bindings referenceBindings) literalIterationBindings(expression ast.Expr, resolve func(ast.Expr) cue.Value) (keys, values []cue.Value, literal bool) {
	if wrapped, ok := expression.(*ast.ParenExpr); ok {
		return bindings.literalIterationBindings(wrapped.X, resolve)
	}
	list, ok := expression.(*ast.ListLit)
	if !ok {
		return nil, nil, false
	}
	for index, element := range list.Elts {
		selected, bound := bindings.selectValues(element, resolve)
		if !bound {
			selected = []cue.Value{resolve(element)}
		}
		for _, value := range selected {
			keys = append(keys, value.Context().Encode(index))
			values = append(values, value)
		}
	}
	return keys, values, true
}

// Follow resolved lexical identities transitively; unused sibling lets do not
// constrain the selected expression merely because their declarations exist.
func referencedLexicalLets(expression ast.Expr) map[*ast.LetClause]bool {
	needed := make(map[*ast.LetClause]bool)
	var visit func(ast.Node)
	visit = func(root ast.Node) {
		ast.Walk(root, func(node ast.Node) bool {
			if _, declaration := node.(*ast.LetClause); declaration {
				return false
			}
			if identifier, ok := node.(*ast.Ident); ok {
				if lexical, ok := identifier.Node.(*ast.LetClause); ok && !needed[lexical] {
					needed[lexical] = true
					visit(lexical.Expr)
				}
			}
			return true
		}, nil)
	}
	visit(expression)
	return needed
}
