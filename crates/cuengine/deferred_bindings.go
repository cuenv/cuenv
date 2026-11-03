package main

import (
	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
)

// These are original CUE values from each actual iteration, not reconstructed
// JSON. Selecting a bound field therefore retains its real source identity.
type referenceBindings map[string][]cue.Value

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
		values, exists := bindings[expression.Name]
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
			if _, nested := declaration.Value.(*ast.StructLit); nested {
				field := *declaration
				field.Value = comprehensionControlBody(field.Value)
				declarations = append(declarations, &field)
			}
		}
	}
	return &ast.StructLit{Elts: declarations}
}
