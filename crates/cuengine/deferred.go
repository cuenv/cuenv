package main

import (
	"fmt"
	"reflect"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/token"
)

// ReferenceEvaluation is an explicit, generic opt-in for callers that lower
// selected native references after evaluation. Deferred entries are verified
// again during strict validation; unrelated incomplete values remain errors.
type ReferenceEvaluation struct {
	Paths         []string            `json:"paths"`
	AbstractPaths []string            `json:"abstractPaths"`
	Deferred      map[string][]string `json:"deferred"`
}

// directReference accepts one reference, optionally unified with an unconstrained
// primitive type. It never looks through arithmetic, interpolation, disjunctions
// containing references, or constraints that would be lost by lowering.
func directReference(value, instance cue.Value) (labels []string, err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			labels = nil
			err = fmt.Errorf("CUE reference extraction failed")
		}
	}()
	return directReferenceDepth(value, instance, 0)
}

// DirectReference preserves the canonical bridge path and structured labels,
// so callers never need to parse or reproduce CUE path formatting.
type DirectReference struct {
	Path          string   `json:"path"`
	Labels        []string `json:"labels"`
	Target        []string `json:"target"`
	PrimitiveType string   `json:"primitiveType"`
	Supported     bool     `json:"supported"`
}

func directReferenceDepth(value, instance cue.Value, depth int) ([]string, error) {
	if depth > 32 {
		return nil, fmt.Errorf("CUE reference nesting exceeds 32 levels")
	}
	root, path := value.ReferencePath()
	if root.Exists() {
		if root != instance {
			return nil, nil
		}
		terminal, err := directReferenceDepth(root.LookupPath(path), instance, depth+1)
		if err != nil {
			return nil, err
		}
		if len(terminal) > 0 {
			return terminal, nil
		}
		return regularLabels(path), nil
	}
	dereferenced := cue.Dereference(value)
	if dereferenced != value {
		return directReferenceDepth(dereferenced, instance, depth+1)
	}
	operation, operands := value.Expr()
	if operation != cue.AndOp {
		return nil, nil
	}
	var target []string
	var constraints []cue.Value
	for _, operand := range operands {
		candidate, err := directReferenceDepth(operand, instance, depth+1)
		if err != nil {
			return nil, err
		}
		if len(candidate) > 0 {
			if target != nil && !reflect.DeepEqual(target, candidate) {
				return nil, nil
			}
			target = candidate
		} else {
			constraints = append(constraints, operand)
		}
	}
	if len(target) == 0 {
		return nil, nil
	}
	primitive := primitiveDeclaration(instance.LookupPath(labelPath(target)))
	if primitive == "" {
		return nil, nil
	}
	for _, constraint := range constraints {
		if constraint.Subsume(instance.Context().CompileString(primitive)) != nil {
			return nil, nil
		}
	}
	return target, nil
}

func labelPath(labels []string) cue.Path {
	selectors := make([]cue.Selector, 0, len(labels))
	for _, label := range labels {
		selectors = append(selectors, cue.Str(label))
	}
	return cue.MakePath(selectors...)
}

func regularLabels(path cue.Path) []string {
	labels := make([]string, 0, len(path.Selectors()))
	for _, selector := range path.Selectors() {
		if selector.LabelType() != cue.StringLabel || selector.ConstraintType() != 0 {
			return nil
		}
		labels = append(labels, selector.Unquoted())
	}
	return labels
}

// A declaration must be exactly one abstract primitive, without a default or
// constraints that would be lost when the provider supplies its runtime value.
func primitiveDeclaration(value cue.Value) string {
	var primitive string
	switch value.IncompleteKind() {
	case cue.StringKind:
		primitive = "string"
	case cue.NumberKind:
		primitive = "number"
	case cue.BoolKind:
		primitive = "bool"
	default:
		return ""
	}
	if _, hasDefault := value.Default(); hasDefault {
		return ""
	}
	if value.Subsume(value.Context().CompileString(primitive)) != nil {
		return ""
	}
	return primitive
}

// expressionReferences also records references inside unsupported expressions.
// Callers can reject those explicitly instead of treating a narrowed reference
// as a literal value after CUE evaluation.
func expressionReferences(value, instance cue.Value, depth int) ([][]string, error) {
	if depth > 32 {
		return nil, fmt.Errorf("CUE reference nesting exceeds 32 levels")
	}
	root, path := value.ReferencePath()
	if root.Exists() {
		if root != instance {
			return nil, nil
		}
		nested, err := expressionReferences(root.LookupPath(path), instance, depth+1)
		if err != nil {
			return nil, err
		}
		if len(nested) > 0 {
			return nested, nil
		}
		labels := regularLabels(path)
		if labels == nil {
			return nil, nil
		}
		return [][]string{labels}, nil
	}
	dereferenced := cue.Dereference(value)
	if dereferenced != value {
		return expressionReferences(dereferenced, instance, depth+1)
	}
	operation, operands := value.Expr()
	if operation == cue.NoOp {
		return rawExpressionReferences(value, instance, depth+1)
	}
	if operation == cue.OrOp {
		// Non-defaulted disjunction operands retain their evaluated scopes.
		// Defaults can hide arms even while Expr still reports OrOp, so
		// those require the retained syntax inspection as well.
		var hasDefault bool
		ast.Walk(value.Syntax(cue.Raw()), func(node ast.Node) bool {
			if unary, ok := node.(*ast.UnaryExpr); ok && unary.Op == token.MUL {
				hasDefault = true
			}
			return !hasDefault
		}, nil)
		if hasDefault {
			return rawExpressionReferences(value, instance, depth+1)
		}
	}
	var references [][]string
	for _, operand := range operands {

		found, err := expressionReferences(operand, instance, depth+1)
		if err != nil {
			return nil, err
		}
		references = append(references, found...)
	}
	return references, nil
}

// Expr can simplify a defaulted union to NoOp, hiding its non-default arms.
// Raw syntax retains those arms. Rebuild only individual reference expressions
// in their original CUE scope and ask ReferencePath again; syntax alone never
// authorizes lowering or supplies a target path.
func rawExpressionReferences(value, instance cue.Value, depth int) ([][]string, error) {
	if depth > 32 {
		return nil, fmt.Errorf("CUE reference nesting exceeds 32 levels")
	}
	expression, err := referenceExpression(value.Syntax(cue.Raw()))
	if err != nil {
		return nil, err
	}
	// Expr also exposes standalone predeclared operands from imported schemas.
	// Rebuilding one in the value's scope can bind it to a quoted provider
	// attribute (for example "number"?) instead of the predeclared type. Only
	// skip an identifier when its evaluated value equals its unscoped meaning;
	// reference paths and dereferences were already inspected by the caller.
	if identifier, ok := expression.(*ast.Ident); ok {
		if _, hasDefault := value.Default(); !hasDefault {
			predeclared := value.Context().BuildExpr(identifier)
			if predeclared.Err() == nil && value.Subsume(predeclared) == nil && predeclared.Subsume(value) == nil {
				return nil, nil
			}
		}
	}
	if (value.Kind() == cue.StructKind || value.Kind() == cue.ListKind) && !rawHasAlternatives(expression) {
		return nil, nil
	}
	var references [][]string
	bindings := make(map[string]bool)
	resolve := func(expression ast.Expr) cue.Value {
		wrapper := ast.NewStruct(ast.NewString(""), expression)
		resolved := value.Context().BuildExpr(wrapper, cue.Scope(value))
		if err := resolved.Err(); err != nil {
			return resolved
		}
		return resolved.LookupPath(cue.MakePath(cue.Str("")))
	}
	var visit func(ast.Expr) error
	var visitLabel func(ast.Label) error
	visitLabel = func(label ast.Label) error {
		switch label := label.(type) {
		case *ast.Ident, *ast.BasicLit:
			return nil
		case *ast.Alias:
			nested, ok := label.Expr.(ast.Label)
			if !ok {
				return fmt.Errorf("CUE reference extraction cannot inspect dynamic field label %T", label.Expr)
			}
			return visitLabel(nested)
		default:
			expression, ok := label.(ast.Expr)
			if !ok {
				return fmt.Errorf("CUE reference extraction cannot inspect field label %T", label)
			}
			return visit(expression)
		}
	}
	visit = func(expression ast.Expr) error {
		if expression == nil {
			return nil
		}
		switch expression := expression.(type) {
		case *ast.BasicLit, *ast.BottomLit:
			return nil
		case *ast.Alias:
			return visit(expression.Expr)
		case *ast.Ellipsis:
			return visit(expression.Type)
		case *ast.SliceExpr:
			if err := visit(expression.X); err != nil {
				return err
			}
			if err := visit(expression.Low); err != nil {
				return err
			}
			return visit(expression.High)
		case *ast.Func:
			for _, argument := range expression.Args {
				if err := visit(argument); err != nil {
					return err
				}
			}
			return visit(expression.Ret)
		case *ast.Comprehension:
			previous := bindings
			bindings = make(map[string]bool, len(previous))
			for name, safe := range previous {
				bindings[name] = safe
			}
			defer func() { bindings = previous }()
			for _, clause := range expression.Clauses {
				switch clause := clause.(type) {
				case *ast.ForClause:
					before := len(references)
					if err := visit(clause.Source); err != nil {
						return err
					}
					source := resolve(clause.Source)
					safe := source.Validate(cue.Concrete(true), cue.Final()) == nil || len(references) == before
					if clause.Key != nil {
						bindings[clause.Key.Name] = true
					}
					if clause.Value != nil {
						bindings[clause.Value.Name] = safe
					}
				case *ast.IfClause:
					if err := visit(clause.Condition); err != nil {
						return err
					}
				case *ast.LetClause:
					before := len(references)
					if err := visit(clause.Expr); err != nil {
						return err
					}
					bindings[clause.Ident.Name] = resolve(clause.Expr).Validate(cue.Concrete(true), cue.Final()) == nil || len(references) == before
				case *ast.TryClause:
					before := len(references)
					if err := visit(clause.Expr); err != nil {
						return err
					}
					if clause.Ident != nil {
						bindings[clause.Ident.Name] = resolve(clause.Expr).Validate(cue.Concrete(true), cue.Final()) == nil || len(references) == before
					}
				default:
					return fmt.Errorf("CUE reference extraction cannot inspect comprehension clause %T", clause)
				}
			}
			if err := visit(expression.Value); err != nil {
				return err
			}
			if expression.Fallback != nil {
				return visit(expression.Fallback.Body)
			}
			return nil
		case *ast.BinaryExpr:
			if err := visit(expression.X); err != nil {
				return err
			}
			return visit(expression.Y)
		case *ast.UnaryExpr:
			return visit(expression.X)
		case *ast.ParenExpr:
			return visit(expression.X)
		case *ast.Interpolation:
			for _, element := range expression.Elts {
				if err := visit(element); err != nil {
					return err
				}
			}
			return nil
		case *ast.CallExpr:
			if err := visit(expression.Fun); err != nil {
				return err
			}
			for _, argument := range expression.Args {
				if err := visit(argument); err != nil {
					return err
				}
			}
			return nil
		case *ast.StructLit:
			for _, declaration := range expression.Elts {
				switch declaration := declaration.(type) {
				case *ast.Field:
					if err := visitLabel(declaration.Label); err != nil {
						return err
					}
					if err := visit(declaration.Value); err != nil {
						return err
					}
				case *ast.EmbedDecl:
					if err := visit(declaration.Expr); err != nil {
						return err
					}
				case *ast.LetClause:
					if err := visit(declaration.Expr); err != nil {
						return err
					}
				case *ast.Attribute:
					// Attribute text does not contain CUE expressions.
				default:
					if expression, ok := declaration.(ast.Expr); ok {
						if err := visit(expression); err != nil {
							return err
						}
					} else {
						return fmt.Errorf("CUE reference extraction cannot inspect declaration %T", declaration)
					}
				}
			}
			return nil
		case *ast.ListLit:
			for _, element := range expression.Elts {
				if err := visit(element); err != nil {
					return err
				}
			}
			return nil
		case *ast.Ident, *ast.SelectorExpr, *ast.IndexExpr:
			base := ast.Expr(expression)
			for {
				switch node := base.(type) {
				case *ast.SelectorExpr:
					base = node.X
				case *ast.IndexExpr:
					if err := visit(node.Index); err != nil {
						return err
					}
					base = node.X
				default:
					goto bindingRoot
				}
			}
		bindingRoot:
			if identifier, ok := base.(*ast.Ident); ok {
				// An imported package cannot reference this instance. Its binding
				// comes from CUE's resolved AST, not the identifier spelling.
				// Index expressions were inspected above before this exclusion.
				if _, imported := identifier.Node.(*ast.ImportSpec); imported {
					return nil
				}
				if safe, bound := bindings[identifier.Name]; bound {
					if safe {
						return nil
					}
					return fmt.Errorf("CUE reference extraction cannot inspect a reference through an incomplete comprehension binding")
				}
			}
			// The empty label has no identifier spelling, so the wrapper
			// cannot shadow names used by the original expression.
			operand := resolve(expression)
			if err := operand.Err(); err != nil {
				return fmt.Errorf("CUE reference extraction from unsimplified expression failed: %w", err)
			}
			// Resolve identifiers before deciding they are plain predeclared
			// expressions. Fields and lets can shadow every predeclared name.
			if _, identifier := expression.(*ast.Ident); identifier {
				root, _ := operand.ReferencePath()
				if !root.Exists() && cue.Dereference(operand) == operand {
					if _, plain := operand.Syntax(cue.Raw()).(*ast.Ident); plain {
						return nil
					}
				}
			}
			found, err := expressionReferences(operand, instance, depth+1)
			if err != nil {
				return err
			}
			references = append(references, found...)
		default:
			return fmt.Errorf("CUE reference extraction cannot inspect expression %T", expression)
		}
		return nil
	}
	if err := visit(expression); err != nil {
		return nil, err
	}
	return references, nil
}

// Syntax returns a file when its value needs imports or auxiliary declarations.
// Imports and package names contain no value expressions; retain all other known
// declarations for inspection. Unknown syntax and declaration shapes fail closed.
func referenceExpression(node ast.Node) (ast.Expr, error) {
	if expression, ok := node.(ast.Expr); ok {
		return expression, nil
	}
	file, ok := node.(*ast.File)
	if !ok {
		return nil, fmt.Errorf("CUE reference extraction cannot inspect syntax %T", node)
	}
	var declarations []ast.Decl
	for _, declaration := range file.Decls {
		switch declaration.(type) {
		case *ast.ImportDecl, *ast.Package:
			// Imported operands are recognized by their resolved ImportSpec
			// binding during traversal; import paths contain no expressions.
		case *ast.Field, *ast.LetClause, *ast.EmbedDecl, *ast.Alias,
			*ast.Attribute, *ast.Ellipsis, *ast.Comprehension:
			declarations = append(declarations, declaration)
		default:
			return nil, fmt.Errorf("CUE reference extraction cannot inspect file declaration %T", declaration)
		}
	}
	if len(declarations) == 1 {
		if embedded, ok := declarations[0].(*ast.EmbedDecl); ok {
			return embedded.Expr, nil
		}
	}
	return &ast.StructLit{Elts: declarations}, nil
}

func rawHasAlternatives(expression ast.Expr) bool {
	switch expression := expression.(type) {
	case *ast.StructLit:
		for _, declaration := range expression.Elts {
			if embedded, ok := declaration.(*ast.EmbedDecl); ok && rawHasAlternatives(embedded.Expr) {
				return true
			}
		}
		return false
	case *ast.BinaryExpr:
		return expression.Op == token.OR || rawHasAlternatives(expression.X) || rawHasAlternatives(expression.Y)
	case *ast.ParenExpr:
		return rawHasAlternatives(expression.X)
	case *ast.UnaryExpr:
		return rawHasAlternatives(expression.X)
	}
	return false
}

func collectDirectReferences(instance cue.Value, paths []projectionPath) (references []DirectReference, err error) {
	references = make([]DirectReference, 0)
	defer func() {
		if recovered := recover(); recovered != nil {
			references = nil
			err = fmt.Errorf("CUE reference extraction failed")
		}
	}()
	var visit func(cue.Value, cue.Path, []string) error
	visit = func(value cue.Value, path cue.Path, labels []string) error {
		if value.Err() != nil {
			return value.Err()
		}
		candidates, err := expressionReferences(value, instance, 0)
		if err != nil {
			return fmt.Errorf("%s: %w", path, err)
		}
		supported, err := directReference(value, instance)
		if err != nil {
			return err
		}
		for _, target := range candidates {
			primitive := primitiveDeclaration(instance.LookupPath(labelPath(target)))
			references = append(references, DirectReference{Path: path.String(), Labels: append([]string{}, labels...), Target: target, PrimitiveType: primitive, Supported: primitive != "" && reflect.DeepEqual(supported, target)})
		}
		if value.Kind() == cue.StructKind {
			fields, err := value.Fields(cue.Definitions(false))
			if err != nil {
				return err
			}
			for fields.Next() {
				selector := fields.Selector()
				if err := visit(fields.Value(), cue.MakePath(append(path.Selectors(), selector)...), append(append([]string{}, labels...), selector.Unquoted())); err != nil {
					return err
				}
			}
		} else if value.Kind() == cue.ListKind {
			items, err := value.List()
			if err != nil {
				return err
			}
			for index := 0; items.Next(); index++ {
				if err := visit(items.Value(), cue.MakePath(append(path.Selectors(), cue.Index(index))...), append(append([]string{}, labels...), fmt.Sprint(index))); err != nil {
					return err
				}
			}
		}
		return nil
	}
	for _, path := range paths {
		value := instance.LookupPath(path.path)
		if value.Exists() {
			if err := visit(value, path.path, path.labels); err != nil {
				return nil, err
			}
		}
	}
	return references, nil
}

func validateAbstractNamespace(value cue.Value, path cue.Path) error {
	if value.Kind() != cue.StructKind {
		return fmt.Errorf("%s: abstract namespace must be a struct", path)
	}
	fields, err := value.Fields(cue.Definitions(false))
	if err != nil {
		return err
	}
	for fields.Next() {
		if primitiveDeclaration(fields.Value()) == "" {
			return fmt.Errorf("%s: abstract declarations must be bare string, number or bool types without defaults or refinements", cue.MakePath(append(path.Selectors(), fields.Selector())...))
		}
	}
	return nil
}

// validateConcreteExcept retains finality (including required fields), checks
// list lengths, and checks every leaf except the caller's exact exemptions.
type concreteValidation struct {
	instance   cue.Value
	paths      []concretePath
	moduleRoot string
}

func validateConcreteExcept(request concreteValidation, options *ReferenceEvaluation) error {
	instance, paths, moduleRoot := request.instance, request.paths, request.moduleRoot
	abstract, err := parseProjectionPaths("abstractPaths", options.AbstractPaths)
	if err != nil {
		return err
	}
	exempt := make(map[string]bool)
	for _, path := range abstract {
		exempt[path.path.String()] = true
	}
	var visit func(cue.Value, cue.Path) error
	visit = func(value cue.Value, path cue.Path) error {
		key := path.String()
		if exempt[key] {
			return validateAbstractNamespace(value, path)
		}
		if expected, ok := options.Deferred[key]; ok {
			actual, err := directReference(value, instance)
			if err != nil {
				return err
			}
			if len(actual) == 0 || !reflect.DeepEqual(actual, expected) {
				return fmt.Errorf("%s: deferred reference does not match the selected CUE reference", path)
			}
			return nil
		}
		switch value.Kind() {
		case cue.StructKind:
			fields, err := value.Fields(cue.Definitions(false))
			if err != nil {
				return err
			}
			for fields.Next() {
				if err := visit(fields.Value(), cue.MakePath(append(path.Selectors(), fields.Selector())...)); err != nil {
					return err
				}
			}
		case cue.ListKind:
			if err := value.Len().Validate(cue.Concrete(true)); err != nil {
				return err
			}
			items, err := value.List()
			if err != nil {
				return err
			}
			for index := 0; items.Next(); index++ {
				if err := visit(items.Value(), cue.MakePath(append(path.Selectors(), cue.Index(index))...)); err != nil {
					return err
				}
			}
		default:
			if err := value.Validate(cue.Concrete(true), cue.Final()); err != nil {
				return err
			}
		}
		return nil
	}
	for _, concrete := range paths {
		value := instance.LookupPath(concrete.path)
		if !value.Exists() {
			return fmt.Errorf("%s: concrete path does not exist", concrete.text)
		}
		if err := value.Validate(cue.Final()); err != nil {
			return fmt.Errorf("%s: %s", concrete.text, errorDetails(err, moduleRoot))
		}
		if err := visit(value, concrete.path); err != nil {
			return fmt.Errorf("%s: %s", concrete.text, errorDetails(err, moduleRoot))
		}
	}
	return nil
}
