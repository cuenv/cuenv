package main

import (
	"errors"
	"fmt"
	"reflect"
	"sort"
	"strings"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/format"
	"cuelang.org/go/cue/parser"
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
	if openBoundSliceSource(value) != nil {
		return nil, nil
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
// constraints that would be lost when its runtime value is supplied.
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
	paths, err := expressionReferencePaths(value, instance, depth)
	if err != nil {
		return nil, err
	}
	var references [][]string
	for _, path := range paths {
		if labels := regularLabels(path); labels != nil {
			references = append(references, labels)
		}
	}
	return references, nil
}

// expressionReferencePaths preserves list selectors for generic specialization;
// the field-only reference inspection above accepts ordinary field paths.
func expressionReferencePaths(value, instance cue.Value, depth int) ([]cue.Path, error) {
	if depth > 32 {
		return nil, fmt.Errorf("CUE reference nesting exceeds 32 levels")
	}
	root, path := value.ReferencePath()
	if root.Exists() {
		if root != instance && fileScopedReference(value) {
			root = instance
		}
		if root != instance {
			if packageRoot(root) && !selectsThroughLet(value) {
				return nil, nil
			}
			return letSelectedPaths(value, instance, depth)
		}
		nested, err := expressionReferencePaths(root.LookupPath(path), instance, depth+1)
		if err != nil {
			return nil, err
		}
		if len(nested) > 0 {
			return nested, nil
		}
		if namesDefinition(path) {
			return nil, nil
		}
		return []cue.Path{path}, nil
	}
	// Selecting through a let reports only the let's evaluated struct as a base,
	// and dereferencing it lands on a declaration with no trace of the selection.
	if selectsThroughLet(value) {
		return letSelectedPaths(value, instance, depth)
	}
	dereferenced := cue.Dereference(value)
	if dereferenced != value {
		return expressionReferencePaths(dereferenced, instance, depth+1)
	}
	if slice := openBoundSliceSource(value); slice != nil {
		return inspectRawReferencePaths(rawReferenceInspection{value: value, instance: instance, expression: slice, depth: depth + 1, originalSource: true})
	}
	operation, operands := value.Expr()
	if operation == cue.NoOp {
		return rawExpressionReferencePaths(value, instance, depth+1)
	}
	// A builtin can read struct arguments that have no path of their own and would
	// be skipped as data below. Everything written in its call counts.
	if operation == cue.CallOp {
		if call, ok := retainedCall(value); ok {
			return inspectRawReferencePaths(rawReferenceInspection{value: value, instance: instance, expression: call, depth: depth + 1, originalSource: true, containersAllowed: true})
		}
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
			return rawExpressionReferencePaths(value, instance, depth+1)
		}
	}
	var references []cue.Path
	for _, operand := range operands {

		found, err := expressionReferencePaths(operand, instance, depth+1)
		if err != nil {
			return nil, err
		}
		references = append(references, found...)
	}
	return references, nil
}

// A selector through a let reports the let as its root, which has no path in the
// instance. Its retained source names the value the let holds; references into an
// imported package have no such binding and cannot reach this instance.
func letSelectedPaths(value, instance cue.Value, depth int) ([]cue.Path, error) {
	expression, identifier, ok := retainedSelection(value)
	isLet := false
	if ok {
		_, isLet = identifier.Node.(*ast.LetClause)
	}
	if !isLet || expression == ast.Expr(identifier) {
		// A concrete value has nothing left to be filled. If a source narrowed it,
		// the expression that did so is inspected and sampled where it is written.
		if value.Validate(cue.Concrete(true), cue.Final()) == nil {
			return nil, nil
		}
		return nil, errUnknownReferenceRoot
	}
	return inspectRawReferencePaths(rawReferenceInspection{value: value, instance: instance, expression: expression, depth: depth + 1, originalSource: true})
}

// selectThroughBinding rewrites C.a.b for let C = source to source.a.b, and reads
// C.a from a literal let C = {a: source}. CUE still binds the result, which keeps
// its own identifiers' declarations.
func selectThroughBinding(expression, binding ast.Expr) (ast.Expr, bool) {
	selector, ok := expression.(*ast.SelectorExpr)
	if !ok {
		if wrapped, isParen := expression.(*ast.ParenExpr); isParen {
			return selectThroughBinding(wrapped.X, binding)
		}
		return nil, false
	}
	if _, root := selector.X.(*ast.Ident); !root {
		base, ok := selectThroughBinding(selector.X, binding)
		if !ok {
			return nil, false
		}
		return selectFrom(base, selector)
	}
	return selectFrom(binding, selector)
}

func selectFrom(base ast.Expr, selector *ast.SelectorExpr) (ast.Expr, bool) {
	if wrapped, isParen := base.(*ast.ParenExpr); isParen {
		return selectFrom(wrapped.X, selector)
	}
	if plainReference(base) {
		selected := *selector
		selected.X = base
		return &selected, true
	}
	literal, ok := base.(*ast.StructLit)
	if !ok {
		return nil, false
	}
	name, _, err := ast.LabelName(selector.Sel)
	if err != nil {
		return nil, false
	}
	for _, declaration := range literal.Elts {
		if field, ok := declaration.(*ast.Field); ok {
			if label, _, err := ast.LabelName(field.Label); err == nil && label == name {
				return field.Value, true
			}
		}
	}
	return nil, false
}

func retainedCall(value cue.Value) (*ast.CallExpr, bool) {
	var original ast.Node = value.Source()
	if field, ok := original.(*ast.Field); ok {
		original = field.Value
	}
	call, ok := original.(*ast.CallExpr)
	if !ok {
		return nil, false
	}
	// close only changes what a struct allows. Its fields are visited at their paths.
	if name, isIdent := call.Fun.(*ast.Ident); isIdent && name.Name == "close" && name.Node == nil {
		return nil, false
	}
	return call, true
}

// plainLetSelection reports whether a selection through lets ends at a plain
// reference. Anything else computes the value it selects, so it is not direct.
func plainLetSelection(value cue.Value) bool {
	expression, _, ok := retainedSelection(value)
	for depth := 0; ok && depth < 32; depth++ {
		base := expression
		for {
			if selector, isSelector := base.(*ast.SelectorExpr); isSelector {
				base = selector.X
				continue
			}
			if wrapped, isParen := base.(*ast.ParenExpr); isParen {
				base = wrapped.X
				continue
			}
			break
		}
		identifier, isIdent := base.(*ast.Ident)
		if !isIdent {
			return false
		}
		binding, isLet := identifier.Node.(*ast.LetClause)
		if !isLet {
			return plainReference(expression)
		}
		expression, ok = selectThroughBinding(expression, binding.Expr)
	}
	return false
}

func selectsThroughLet(value cue.Value) bool {
	expression, identifier, ok := retainedSelection(value)
	if !ok || expression == ast.Expr(identifier) {
		return false
	}
	_, isLet := identifier.Node.(*ast.LetClause)
	return isLet
}

// retainedSelection returns the source expression behind a value and the
// identifier its selectors are rooted in.
func retainedSelection(value cue.Value) (ast.Expr, *ast.Ident, bool) {
	var original ast.Node = value.Source()
	if field, ok := original.(*ast.Field); ok {
		original = field.Value
	}
	expression, ok := original.(ast.Expr)
	if !ok {
		return nil, nil, false
	}
	base := expression
	for {
		switch node := base.(type) {
		case *ast.SelectorExpr:
			base = node.X
			continue
		case *ast.ParenExpr:
			base = node.X
			continue
		case *ast.Ident:
			return expression, node, true
		}
		return nil, nil, false
	}
}

// An embedded #Root reports a root without a source for a name declared at file
// level. Treat it as this instance, where its fields are checked like any other.
func fileScopedReference(value cue.Value) bool {
	identifier, ok := value.Source().(*ast.Ident)
	if !ok {
		return false
	}
	_, file := identifier.Scope.(*ast.File)
	return file
}

// A package root holds only its own declarations. It cannot hold a runtime source
// of this instance: sources are filled on this instance's own paths.
func packageRoot(root cue.Value) bool {
	if root.BuildInstance() != nil {
		return true
	}
	_, file := root.Source().(*ast.File)
	return file
}

// A reference rooted in neither this instance, an imported package nor a let has
// nothing to establish independence from, so it can never be reported as none.
var errUnknownReferenceRoot = errors.New("CUE reference has an unknown root")

// sourceTargets follows a reference toward everything it was unified from, stopping
// each branch at the first declared source. Fills flow from a source to its aliases
// and never back, so the walk is directional: with a: _s, a target already at a
// declared path under a stays there even though it aliases _s.
func sourceTargets(path cue.Path, instance cue.Value, sources []cue.Path) []cue.Path {
	seen := make(map[string]bool)
	found := make(map[string]cue.Path)
	var walk func(cue.Path, int)
	walk = func(candidate cue.Path, depth int) {
		if depth > 32 || seen[candidate.String()] || len(candidate.Selectors()) == 0 {
			return
		}
		seen[candidate.String()] = true
		for _, source := range sources {
			if pathWithin(candidate, source) {
				found[candidate.String()] = candidate
				return
			}
		}
		selectors := candidate.Selectors()
		for length := len(selectors); length >= 1; length-- {
			prefix := instance.LookupPath(cue.MakePath(selectors[:length]...))
			for _, reference := range containerReferences(prefix, instance, 0) {
				walk(cue.MakePath(append(append([]cue.Selector{}, reference.Selectors()...), selectors[length:]...)...), depth+1)
			}
		}
	}
	walk(path, 0)
	targets := make([]cue.Path, 0, len(found))
	for _, target := range found {
		targets = append(targets, target)
	}
	sort.Slice(targets, func(left, right int) bool { return targets[left].String() < targets[right].String() })
	return targets
}

// A definition is a schema, never a runtime source. Its fields constrain what is
// unified with it, and references it holds were already followed.
func namesDefinition(path cue.Path) bool {
	for _, selector := range path.Selectors() {
		if selector.IsDefinition() {
			return true
		}
	}
	return false
}

// declaredPaths parses an explicit source list. Nil discovers bare primitives.
func declaredPaths(sources *[]valuePath) []cue.Path {
	if sources == nil {
		return nil
	}
	paths := make([]cue.Path, 0, len(*sources))
	for _, typed := range *sources {
		if path, err := typed.cuePath(); err == nil {
			paths = append(paths, path)
		}
	}
	return paths
}

// containerReferences lists fields a value is directly unified from.
func containerReferences(value, instance cue.Value, depth int) (references []cue.Path) {
	if depth > 32 {
		return nil
	}
	defer func() {
		// Dereference and Expr can panic on values a comprehension produced.
		if recover() != nil {
			references = nil
		}
	}()
	root, path := value.ReferencePath()
	if root.Exists() {
		if root == instance {
			return []cue.Path{path}
		}
		return nil
	}
	if dereferenced := cue.Dereference(value); dereferenced != value {
		return containerReferences(dereferenced, instance, depth+1)
	}
	if openBoundSliceSource(value) != nil {
		return nil
	}
	operation, operands := value.Expr()
	switch operation {
	case cue.AndOp, cue.OrOp:
	case cue.CallOp:
		// close(source), or([source]) and and([source]) copy what they are given.
		if len(operands) > 0 {
			operands = operands[1:]
		}
	default:
		return nil
	}
	for _, operand := range operands {
		references = append(references, containerReferences(operand, instance, depth+1)...)
	}
	return references
}

// The pinned evaluator's Expr constructs operands for absent slice bounds and
// panics on the nil operand. Inspect retained source in its original scope,
// without inventing bounds or replacing CUE's own evaluation of the slice.
func openBoundSliceSource(value cue.Value) *ast.SliceExpr {
	node := value.Source()
	for {
		switch source := node.(type) {
		case *ast.Field:
			node = source.Value
		case *ast.ParenExpr:
			node = source.X
		case *ast.Alias:
			node = source.Expr
		case *ast.SliceExpr:
			if source.Low == nil || source.High == nil {
				return source
			}
			return nil
		default:
			return nil
		}
	}
}

// Expr can simplify a defaulted union to NoOp, hiding its non-default arms.
// Raw syntax retains those arms. Rebuild only individual reference expressions
// in their original CUE scope and ask ReferencePath again; syntax alone never
// authorizes lowering or supplies a target path.
func rawExpressionReferencePaths(value, instance cue.Value, depth int) ([]cue.Path, error) {
	expression, err := referenceExpression(value.Syntax(cue.Raw()))
	if err != nil {
		return nil, err
	}
	if (value.Kind() == cue.StructKind || value.Kind() == cue.ListKind) && !rawHasAlternatives(expression) {
		return nil, nil
	}
	// Some Expr operands become bottom solely because CUE discarded their
	// iteration scope. Reinspect their original syntax with those bindings;
	// ordinary valid operands retain their existing evaluated scopes.
	if value.Err() != nil {
		if original, ok := value.Source().(ast.Expr); ok {
			if controls := structuralAncestors(instance.BuildInstance())[original]; len(controls) > 0 {
				scoped, err := expressionWithIterationBindings(original, controls)
				if err != nil {
					return nil, err
				}
				return inspectRawReferencePaths(rawReferenceInspection{value: value, instance: instance, expression: scoped, depth: depth, originalSource: true})
			}
		}
	}
	found, err := inspectRawReferencePaths(rawReferenceInspection{value: value, instance: instance, expression: expression, depth: depth})
	if err == nil {
		return found, nil
	}
	// Raw syntax drops lexical declarations and source files. Retained source
	// still has both, and is what the raw form was simplified from.
	var original ast.Node = value.Source()
	switch node := original.(type) {
	case *ast.Field:
		original = node.Value
	case *ast.LetClause:
		original = node.Expr
	}
	if retained, ok := original.(ast.Expr); ok {
		// An identifier here resolves through its declaration, not the value's scope.
		if identifier, ok := retained.(*ast.Ident); ok {
			binding, isLet := identifier.Node.(*ast.LetClause)
			if !isLet {
				return nil, err
			}
			retained = binding.Expr
		}
		return inspectRawReferencePaths(rawReferenceInspection{value: value, instance: instance, expression: retained, depth: depth, originalSource: true})
	}
	return nil, err
}

type rawReferenceInspection struct {
	value, instance cue.Value
	expression      ast.Expr
	depth           int
	originalSource  bool
	// containersAllowed reads a let being selected from, not a control over it.
	containersAllowed bool
}

func inspectRawReferencePaths(request rawReferenceInspection) ([]cue.Path, error) {
	value, instance, expression, depth := request.value, request.instance, request.expression, request.depth
	if depth > 32 {
		return nil, fmt.Errorf("CUE reference nesting exceeds 32 levels")
	}
	// Expr also exposes standalone predeclared operands from imported schemas.
	// Rebuilding one in the value's scope can bind it to a quoted field
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
	var references []cue.Path
	bindings := make(map[referenceBindingName]bool)
	boundValues := make(referenceBindings)
	allowContainers := 0
	if request.containersAllowed {
		allowContainers = 1
	}
	resolve := func(expression ast.Expr) cue.Value {
		resolutionScope := value
		owner, known := instanceIdentity(instance, expression)
		original := expression
		// Raw syntax has no source file. A hidden name there could bind to another
		// package's field of the same name, so it must go through retained source.
		if !known && namesHiddenField(expression) {
			return value.Context().CompileString("_|_")
		}
		if request.originalSource {
			base := expression
			for {
				switch node := base.(type) {
				case *ast.SelectorExpr:
					base = node.X
				case *ast.IndexExpr:
					base = node.X
				default:
					goto originalBinding
				}
			}
		originalBinding:
			if identifier, ok := base.(*ast.Ident); ok && identifier.Scope != nil {
				resolvedScope, err := bindingScope(value, instance, identifier)
				if err != nil {
					return value.Context().CompileString("_|_")
				}
				resolutionScope = resolvedScope
			}
			// Original AST identifiers are bound to their source-file nodes,
			// which BuildExpr cannot transplant into another AST. Reparse only
			// the expression being resolved and let CUE bind it in the original
			// value scope. Imported identifiers were excluded by their resolved
			// ImportSpec before reaching this function.
			encoded, err := format.Node(withDeclaredLabel(expression))
			if err != nil {
				return value.Context().CompileString("_|_")
			}
			parsed, err := parser.ParseExpr("", encoded)
			if err != nil {
				return value.Context().CompileString("_|_")
			}
			expression = parsed
		}
		wrapper := ast.NewStruct(ast.NewString(""), expression)
		resolved := value.Context().BuildExpr(wrapper, cue.Scope(resolutionScope), cue.ImportPath(owner))
		if err := resolved.Err(); err != nil {
			// A disjunction cannot act as a compile scope. Its declaring field is
			// already identified, so select it instead of compiling the name.
			if selected, ok := selectDeclared(resolutionScope, instance, original); ok {
				return selected
			}
			return resolved
		}
		return resolved.LookupPath(cue.MakePath(cue.Str("")))
	}
	var visit func(ast.Expr) error
	bind := func(name string, expression ast.Expr, declaration ast.Node) error {
		values, bound := boundValues.selectValues(expression, resolve)
		if !bound {
			resolved := resolve(expression)
			if resolved.Err() == nil {
				values, bound = []cue.Value{originalReferenceValue(resolved, instance)}, true
			}
		}
		allowBindingContainer := bound && plainReference(expression)
		if allowBindingContainer {
			allowContainers++
		}
		before := len(references)
		err := visit(expression)
		if allowBindingContainer {
			allowContainers--
		}
		if err != nil {
			return err
		}
		safe := request.originalSource || resolve(expression).Validate(cue.Concrete(true), cue.Final()) == nil || len(references) == before
		bindings[referenceBindingName{name, declaration}] = safe
		bindings[referenceBindingName{name, nil}] = safe
		if bound {
			boundValues[referenceBindingName{name, declaration}] = values
			boundValues[referenceBindingName{name, nil}] = values
		}
		return nil
	}
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
			previousValues := boundValues
			bindings = make(map[referenceBindingName]bool, len(previous))
			boundValues = make(referenceBindings, len(previousValues))
			for name, safe := range previous {
				bindings[name] = safe
			}
			for name, values := range previousValues {
				boundValues[name] = values
			}
			defer func() { bindings, boundValues = previous, previousValues }()
			for _, clause := range expression.Clauses {
				switch clause := clause.(type) {
				case *ast.ForClause:
					before := len(references)
					allowSourceContainer := plainIterationSource(clause.Source)
					if allowSourceContainer {
						allowContainers++
					}
					err := visit(clause.Source)
					if allowSourceContainer {
						allowContainers--
					}
					if err != nil {
						return err
					}
					source := resolve(clause.Source)
					collections, bound := boundValues.selectValues(clause.Source, resolve)
					if !bound {
						collections = []cue.Value{source}
					}
					keys, values, literal := boundValues.literalIterationBindings(clause.Source, resolve)
					if !literal {
						for _, collection := range collections {
							iterationKeys, iterationValues := iterationBindings(collection, instance)
							keys, values = append(keys, iterationKeys...), append(values, iterationValues...)
						}
					}
					safe := source.Validate(cue.Concrete(true), cue.Final()) == nil || len(references) == before
					if clause.Key != nil {
						bindings[referenceBindingName{clause.Key.Name, clause.Key}] = true
						bindings[referenceBindingName{clause.Key.Name, nil}] = true
						boundValues[referenceBindingName{clause.Key.Name, clause.Key}] = keys
						boundValues[referenceBindingName{clause.Key.Name, nil}] = keys
					}
					if clause.Value != nil {
						bindings[referenceBindingName{clause.Value.Name, clause.Value}] = safe
						bindings[referenceBindingName{clause.Value.Name, nil}] = safe
						boundValues[referenceBindingName{clause.Value.Name, clause.Value}] = values
						boundValues[referenceBindingName{clause.Value.Name, nil}] = values
					}
				case *ast.IfClause:
					if err := visit(clause.Condition); err != nil {
						return err
					}
				case *ast.LetClause:
					if err := bind(clause.Ident.Name, clause.Expr, clause.Ident); err != nil {
						return err
					}
				case *ast.TryClause:
					before := len(references)
					if err := visit(clause.Expr); err != nil {
						return err
					}
					if clause.Ident != nil {
						safe := resolve(clause.Expr).Validate(cue.Concrete(true), cue.Final()) == nil || len(references) == before
						bindings[referenceBindingName{clause.Ident.Name, clause.Ident}] = safe
						bindings[referenceBindingName{clause.Ident.Name, nil}] = safe
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
			_, leftBottom := expression.X.(*ast.BottomLit)
			_, rightBottom := expression.Y.(*ast.BottomLit)
			presence := (leftBottom && plainReference(expression.Y) || rightBottom && plainReference(expression.X)) && (expression.Op == token.EQL || expression.Op == token.NEQ)
			if presence {
				allowContainers++
				defer func() { allowContainers-- }()
			}
			if err := visit(expression.X); err != nil {
				return err
			}
			return visit(expression.Y)
		case *ast.UnaryExpr:
			return visit(expression.X)
		case *ast.PostfixExpr:
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
			// A predeclared builtin resolves to nothing in this instance.
			if identifier, ok := expression.Fun.(*ast.Ident); !ok || identifier.Node != nil {
				if err := visit(expression.Fun); err != nil {
					return err
				}
			}
			if identifier, ok := expression.Fun.(*ast.Ident); ok && identifier.Name == "len" && identifier.Node == nil && len(expression.Args) == 1 && plainReference(expression.Args[0]) {
				allowContainers++
				defer func() { allowContainers-- }()
			}
			for _, argument := range expression.Args {
				if err := visit(argument); err != nil {
					return err
				}
			}
			return nil
		case *ast.StructLit:
			previous, previousValues := bindings, boundValues
			bindings = make(map[referenceBindingName]bool, len(previous))
			boundValues = make(referenceBindings, len(previousValues))
			for name, safe := range previous {
				bindings[name] = safe
			}
			for name, values := range previousValues {
				boundValues[name] = values
			}
			defer func() { bindings, boundValues = previous, previousValues }()
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
					if err := bind(declaration.Ident.Name, declaration.Expr, declaration); err != nil {
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
			if _, identifier := base.(*ast.Ident); !identifier {
				// A computed base (for example a builtin call returning a
				// list) is not a native selector. Inspect its inputs in place;
				// rebuilding the whole call would lose imports and loop binds.
				switch selected := expression.(type) {
				case *ast.IndexExpr:
					return visit(selected.X)
				case *ast.SelectorExpr:
					return visit(selected.X)
				}
			}
			if identifier, ok := base.(*ast.Ident); ok {
				// An imported package cannot reference this instance. Its binding
				// comes from CUE's resolved AST, not the identifier spelling.
				// Index expressions were inspected above before this exclusion.
				if _, imported := identifier.Node.(*ast.ImportSpec); imported {
					return nil
				}
				// A local field can shadow an iteration variable. Its resolved
				// AST identity, rather than its spelling, selects that value.
				if request.originalSource {
					key := bindingName(identifier)
					if _, exists := boundValues[key]; !exists {
						if field, ok := identifier.Node.(ast.Expr); ok && len(structuralAncestors(instance.BuildInstance())[identifier.Scope]) > 0 {
							selected, bound := boundValues.selectValues(field, resolve)
							if !bound {
								resolved := resolve(field)
								if resolved.Err() != nil {
									return fmt.Errorf("CUE shadowing field could not be resolved")
								}
								selected = []cue.Value{originalReferenceValue(resolved, instance)}
							}
							boundValues[key] = selected
						}
					}
				}
				// Selecting through a let reads what the let holds, not the whole of it.
				if binding, ok := identifier.Node.(*ast.LetClause); ok && request.originalSource {
					if _, selection := expression.(*ast.Ident); !selection {
						if selected, ok := selectThroughBinding(expression, binding.Expr); ok {
							return visit(selected)
						}
					}
				}
				if values, bound := boundValues.selectValues(expression, resolve); bound {
					for _, operand := range values {
						if request.originalSource && allowContainers == 0 && (operand.Kind() == cue.StructKind || operand.Kind() == cue.ListKind) && operand.Validate(cue.Concrete(true)) != nil {
							return fmt.Errorf("CUE control uses an incomplete bound container")
						}
						found, err := expressionReferencePaths(operand, instance, depth+1)
						if err != nil {
							return err
						}
						references = append(references, found...)
						path := cue.MakePath(valuePathSelectors(operand)...)
						if len(path.Selectors()) > 0 && instance.LookupPath(path) == operand {
							references = append(references, path)
						}
					}
					return nil
				}
				if _, bound := boundValues[bindingName(identifier)]; bound {
					return fmt.Errorf("CUE control binding selector could not be resolved")
				}
				if safe, bound := bindings[bindingName(identifier)]; bound {
					if safe {
						return nil
					}
					return fmt.Errorf("CUE reference extraction cannot inspect an incomplete comprehension binding")
				}
				if binding, ok := identifier.Node.(*ast.LetClause); ok && request.originalSource {
					// Every name in the binding carries its own lexical declaration,
					// so an unmatched let scope only loses a resolution starting point.
					letScope, err := originalBindingScope(value, instance, identifier.Scope)
					if err != nil {
						letScope = value
					}
					_, whole := expression.(*ast.Ident)
					found, err := inspectRawReferencePaths(rawReferenceInspection{value: letScope, instance: instance, expression: binding.Expr, depth: depth + 1, originalSource: true, containersAllowed: !whole})
					if err != nil {
						return err
					}
					references = append(references, found...)
					return nil
				}
			}
			// The empty label has no identifier spelling, so the wrapper
			// cannot shadow names used by the original expression.
			// x given to (#F & {in: x}).out exists only on the instantiation's own
			// conjuncts: inspect each of them for what in was given.
			if identifier, ok := expression.(*ast.Ident); ok && request.originalSource && identifier.Scope != nil {
				if conjuncts := selectedBaseOperands(value, instance, identifier.Scope); len(conjuncts) > 0 {
					selector := identifierSelector(identifier, instance)
					for _, conjunct := range conjuncts {
						if child := conjunct.LookupPath(cue.MakePath(selector)); child.Exists() {
							found, err := expressionReferencePaths(child, instance, depth+1)
							if err != nil {
								return err
							}
							references = append(references, found...)
						}
					}
					return nil
				}
			}
			operand := resolve(expression)
			// A presence test of a field that does not exist reads nothing.
			if allowContainers > 0 && operand.Err() != nil && plainReference(expression) {
				return nil
			}
			if err := operand.Err(); err != nil {
				if identifier, ok := expression.(*ast.Ident); ok && request.originalSource && identifier.Scope != nil {
					declaring, scopeErr := bindingScope(value, instance, identifier)
					selector := identifierSelector(identifier, instance)
					if scopeErr == nil && !declaring.LookupPath(cue.MakePath(selector)).Exists() && declaring.LookupPath(cue.MakePath(selector.Optional())).Exists() {
						// Absent optional fields cannot be filled. Native CUE still
						// evaluates their presence guards and enclosing controls.
						return nil
					}
				}
				return fmt.Errorf("CUE reference extraction from unsimplified expression failed: %w", err)
			}
			if request.originalSource && allowContainers == 0 && (operand.Kind() == cue.StructKind || operand.Kind() == cue.ListKind) && operand.Validate(cue.Concrete(true)) != nil {
				return fmt.Errorf("CUE control uses an incomplete container")
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
			found, err := expressionReferencePaths(operand, instance, depth+1)
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

// Hidden names belong to the package declaring them. The expression's source file
// names that package; reparsing has to keep the identity to keep the binding.
func instanceIdentity(instance cue.Value, expression ast.Expr) (string, bool) {
	root := instance.BuildInstance()
	filename := expression.Pos().Filename()
	if filename == "" {
		return root.ID(), false
	}
	seen := make(map[*build.Instance]bool)
	var find func(*build.Instance) string
	find = func(candidate *build.Instance) string {
		if candidate == nil || seen[candidate] {
			return ""
		}
		seen[candidate] = true
		for _, file := range candidate.Files {
			if file.Filename == filename {
				return candidate.ID()
			}
		}
		for _, imported := range candidate.Imports {
			if found := find(imported); found != "" {
				return found
			}
		}
		return ""
	}
	if found := find(root); found != "" {
		return found, true
	}
	return root.ID(), false
}

func namesHiddenField(expression ast.Expr) bool {
	found := false
	ast.Walk(expression, func(node ast.Node) bool {
		if identifier, ok := node.(*ast.Ident); ok && strings.HasPrefix(identifier.Name, "_") {
			found = true
		}
		return !found
	}, nil)
	return found
}
