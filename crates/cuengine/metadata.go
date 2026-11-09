package main

import (
	"fmt"
	"path/filepath"
	"strings"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/token"
)

// ValueMeta holds source location metadata for a concrete value
type ValueMeta struct {
	Directory           string `json:"directory"`
	Filename            string `json:"filename"`
	Line                int    `json:"line"`
	DefinitionDirectory string `json:"definitionDirectory,omitempty"`
	DefinitionFilename  string `json:"definitionFilename,omitempty"`
	DefinitionLine      int    `json:"definitionLine,omitempty"`
	Reference           string `json:"reference,omitempty"` // If this value is a reference, the path it refers to
}

// makeMetaKey creates a path-based key for the meta map.
// Format: "instancePath/fieldPath" (e.g., "./env.FOO", "projects/api/env.DATABASE_URL")
func makeMetaKey(instancePath, fieldPath string) string {
	if instancePath == "." {
		return "./" + fieldPath
	}
	return instancePath + "/" + fieldPath
}

// extractFieldMetaSeparate walks the AST to extract source positions for all fields
// and returns them as a separate map (not inline with values).
// Keys are formatted as "instancePath/fieldPath" for correlation with values.
func extractFieldMetaSeparate(inst *build.Instance, moduleRoot, instancePath string) map[string]ValueMeta {
	positions := make(map[string]ValueMeta)

	for _, f := range inst.Files {
		// Calculate relative path from moduleRoot for the filename
		relPath := f.Filename
		if moduleRoot != "" && strings.HasPrefix(f.Filename, moduleRoot) {
			relPath = strings.TrimPrefix(f.Filename, moduleRoot)
			relPath = strings.TrimPrefix(relPath, string(filepath.Separator))
		}
		if relPath == "" {
			relPath = filepath.Base(f.Filename)
		}

		// The instance directory relative to moduleRoot. instancePath is the
		// result key, which also names the package when every package in a
		// directory is evaluated, so the directory is derived separately.
		dir := relativeInstancePath(moduleRoot, inst.Dir)

		for _, decl := range f.Decls {
			switch d := decl.(type) {
			case *ast.Field:
				label, _, _ := ast.LabelName(d.Label)
				extractFieldMetaRecursive(d, label, relPath, dir, instancePath, positions)
			case *ast.EmbedDecl:
				extractFieldMetaFromExpr(d.Expr, "", relPath, dir, instancePath, positions)
			}
		}
	}

	return positions
}

// extractValueMetaSeparate walks evaluated values to extract the source
// position of the concrete value. This differs from extractFieldMetaSeparate:
// field meta describes the binding/caller location, while value meta describes
// where the imported or referenced value was originally defined.
func extractValueMetaSeparate(v cue.Value, moduleRoot, instancePath string) map[string]ValueMeta {
	positions := make(map[string]ValueMeta)
	collector := valueMetaCollector{
		moduleRoot:   moduleRoot,
		instancePath: instancePath,
		positions:    positions,
	}
	collector.walk(v, "")
	return positions
}

type valueMetaCollector struct {
	moduleRoot   string
	instancePath string
	positions    map[string]ValueMeta
}

func (c valueMetaCollector) walk(v cue.Value, fieldPath string) {
	if v.Err() != nil {
		return
	}

	if fieldPath != "" {
		if meta, ok := valueDefinitionMeta(v, c.moduleRoot); ok {
			c.positions[makeMetaKey(c.instancePath, fieldPath)] = meta
		}
	}

	switch v.Kind() {
	case cue.StructKind:
		iter, _ := v.Fields(cue.Definitions(false))
		for iter.Next() {
			label := iter.Label()
			if strings.HasPrefix(label, "_") {
				continue
			}
			childPath := label
			if fieldPath != "" {
				childPath = fieldPath + "." + label
			}
			c.walk(iter.Value(), childPath)
		}
	case cue.ListKind:
		list, _ := v.List()
		for i := 0; list.Next(); i++ {
			childPath := fmt.Sprintf("%s[%d]", fieldPath, i)
			c.walk(list.Value(), childPath)
		}
	}
}

func valueDefinitionMeta(v cue.Value, moduleRoot string) (ValueMeta, bool) {
	if root, path := safeReferenceRootPath(v); root.Exists() {
		referenced := root.LookupPath(path)
		if referenced.Exists() && referenced.Err() == nil {
			if meta, ok := valueMetaFromPosition(referenced.Pos(), moduleRoot); ok {
				return meta, true
			}
		}
	}

	return valueMetaFromPosition(v.Pos(), moduleRoot)
}

func valueMetaFromPosition(pos token.Pos, moduleRoot string) (ValueMeta, bool) {
	if !pos.IsValid() {
		return ValueMeta{}, false
	}

	filename := pos.Filename()
	if filename == "" {
		return ValueMeta{}, false
	}

	relPath := filename
	if moduleRoot != "" && strings.HasPrefix(filename, moduleRoot) {
		relPath = strings.TrimPrefix(filename, moduleRoot)
		relPath = strings.TrimPrefix(relPath, string(filepath.Separator))
	}
	if relPath == "" {
		relPath = filepath.Base(filename)
	}

	dir := filepath.Dir(relPath)
	if dir == "" {
		dir = "."
	}

	return ValueMeta{
		DefinitionDirectory: dir,
		DefinitionFilename:  relPath,
		DefinitionLine:      pos.Line(),
	}, true
}

func safeReferenceRootPath(v cue.Value) (root cue.Value, path cue.Path) {
	defer func() {
		if r := recover(); r != nil {
			root = cue.Value{}
			path = cue.Path{}
		}
	}()

	root, path = v.ReferencePath()
	return root, path
}

// extractFieldMetaRecursive recursively extracts field metadata into the separate map
func extractFieldMetaRecursive(field *ast.Field, fieldPath, filename, directory, instancePath string, positions map[string]ValueMeta) {
	pos := field.Pos()
	metaKey := makeMetaKey(instancePath, fieldPath)
	positions[metaKey] = ValueMeta{
		Directory: directory,
		Filename:  filename,
		Line:      pos.Line(),
	}

	extractFieldMetaFromExpr(field.Value, fieldPath, filename, directory, instancePath, positions)
}

func extractFieldMetaFromExpr(expr ast.Expr, fieldPath, filename, directory, instancePath string, positions map[string]ValueMeta) {
	if expr == nil {
		return
	}

	switch e := expr.(type) {
	case *ast.StructLit:
		for _, elem := range e.Elts {
			switch child := elem.(type) {
			case *ast.Field:
				childLabel, _, _ := ast.LabelName(child.Label)
				childPath := childLabel
				if fieldPath != "" {
					childPath = fieldPath + "." + childLabel
				}
				extractFieldMetaRecursive(child, childPath, filename, directory, instancePath, positions)
			case *ast.EmbedDecl:
				extractFieldMetaFromExpr(child.Expr, fieldPath, filename, directory, instancePath, positions)
			}
		}
	case *ast.ListLit:
		for i, elem := range e.Elts {
			indexPath := fmt.Sprintf("%s[%d]", fieldPath, i)
			extractFieldMetaFromExpr(elem, indexPath, filename, directory, instancePath, positions)
		}
	case *ast.BinaryExpr:
		extractFieldMetaFromExpr(e.X, fieldPath, filename, directory, instancePath, positions)
		extractFieldMetaFromExpr(e.Y, fieldPath, filename, directory, instancePath, positions)
	case *ast.UnaryExpr:
		extractFieldMetaFromExpr(e.X, fieldPath, filename, directory, instancePath, positions)
	case *ast.ParenExpr:
		extractFieldMetaFromExpr(e.X, fieldPath, filename, directory, instancePath, positions)
	}
}

// extractReferencesFromAST walks the AST to find reference identifiers.
// This is necessary because CUE resolves references during evaluation,
// so we need to look at the source AST to find reference information.
// Returns a map of field path -> reference identifier name
func extractReferencesFromAST(inst *build.Instance, instancePath string) map[string]string {
	refs := make(map[string]string)

	for _, f := range inst.Files {
		for _, decl := range f.Decls {
			switch d := decl.(type) {
			case *ast.Field:
				label, _, _ := ast.LabelName(d.Label)
				extractReferencesFromField(d, label, instancePath, refs)
			case *ast.EmbedDecl:
				// Handle embedded declarations like `schema.#Project & {...}`
				// These appear at the top level without a field name
				extractReferencesFromExpr(d.Expr, "", instancePath, refs)
			}
		}
	}

	return refs
}

// extractReferencesFromField recursively extracts reference identifiers from AST fields
func extractReferencesFromField(field *ast.Field, fieldPath, instancePath string, refs map[string]string) {
	// Recurse into the field value (handles all expression types)
	extractReferencesFromExpr(field.Value, fieldPath, instancePath, refs)
}

// extractReferencesFromExpr extracts reference identifiers from an AST expression
func extractReferencesFromExpr(expr ast.Expr, fieldPath, instancePath string, refs map[string]string) {
	if expr == nil {
		return
	}

	switch e := expr.(type) {
	case *ast.ListLit:
		// Check list elements for references
		for i, elem := range e.Elts {
			indexPath := fmt.Sprintf("%s[%d]", fieldPath, i)

			// Direct identifier reference (e.g., `dependsOn: [build]`)
			// Only record if it looks like a task reference (not a built-in type)
			if ident, ok := elem.(*ast.Ident); ok {
				// Skip CUE built-in types and reserved identifiers
				if !isBuiltinType(ident.Name) {
					metaKey := makeMetaKey(instancePath, indexPath)
					refs[metaKey] = ident.Name
				}
			}

			// Selector expression (e.g., `dependsOn: [tasks.build]`)
			if sel, ok := elem.(*ast.SelectorExpr); ok {
				refPath := selectorToPath(sel)
				if refPath != "" {
					metaKey := makeMetaKey(instancePath, indexPath)
					refs[metaKey] = refPath
				}
			}

			// Recurse into nested expressions
			extractReferencesFromExpr(elem, indexPath, instancePath, refs)
		}

	case *ast.StructLit:
		// Recurse into struct fields
		for _, elem := range e.Elts {
			if childField, ok := elem.(*ast.Field); ok {
				childLabel, _, _ := ast.LabelName(childField.Label)
				var childPath string
				if fieldPath != "" {
					childPath = fieldPath + "." + childLabel
				} else {
					childPath = childLabel
				}
				extractReferencesFromField(childField, childPath, instancePath, refs)
			}
		}

	case *ast.BinaryExpr:
		// Handle binary expressions like `#Task & {...}`
		// Recurse into both operands
		extractReferencesFromExpr(e.X, fieldPath, instancePath, refs)
		extractReferencesFromExpr(e.Y, fieldPath, instancePath, refs)

	case *ast.UnaryExpr:
		// Handle unary expressions
		extractReferencesFromExpr(e.X, fieldPath, instancePath, refs)

	case *ast.ParenExpr:
		// Handle parenthesized expressions
		extractReferencesFromExpr(e.X, fieldPath, instancePath, refs)

	case *ast.CallExpr:
		// Handle call expressions - recurse into arguments
		for i, arg := range e.Args {
			argPath := fmt.Sprintf("%s.arg%d", fieldPath, i)
			extractReferencesFromExpr(arg, argPath, instancePath, refs)
		}

	case *ast.Ident:
		// Direct identifier reference at field level
		// Only record if it looks like a reference (not a built-in type)
		if !isBuiltinType(e.Name) {
			metaKey := makeMetaKey(instancePath, fieldPath)
			refs[metaKey] = e.Name
		}

	case *ast.SelectorExpr:
		// Selector expression at field level
		refPath := selectorToPath(e)
		if refPath != "" {
			metaKey := makeMetaKey(instancePath, fieldPath)
			refs[metaKey] = refPath
		}

		// Recurse into complex selector bases so we don't miss references
		// inside expressions like `(#Template & { dependsOn: [build] }).output`.
		switch e.X.(type) {
		case *ast.ParenExpr, *ast.BinaryExpr, *ast.StructLit, *ast.ListLit, *ast.CallExpr, *ast.UnaryExpr, *ast.IndexExpr, *ast.SliceExpr:
			extractReferencesFromExpr(e.X, fieldPath, instancePath, refs)
		}

	case *ast.IndexExpr:
		// Handle index expressions
		extractReferencesFromExpr(e.X, fieldPath, instancePath, refs)
		extractReferencesFromExpr(e.Index, fieldPath, instancePath, refs)

	case *ast.SliceExpr:
		// Handle slice expressions
		extractReferencesFromExpr(e.X, fieldPath, instancePath, refs)
		extractReferencesFromExpr(e.Low, fieldPath, instancePath, refs)
		extractReferencesFromExpr(e.High, fieldPath, instancePath, refs)
	}
}

// isBuiltinType returns true if the identifier is a CUE built-in type
func isBuiltinType(name string) bool {
	builtins := map[string]bool{
		"string": true, "bytes": true, "bool": true,
		"int": true, "int8": true, "int16": true, "int32": true, "int64": true,
		"uint": true, "uint8": true, "uint16": true, "uint32": true, "uint64": true,
		"float": true, "float32": true, "float64": true,
		"number": true, "null": true, "_": true, "_|_": true,
		"true": true, "false": true,
	}
	return builtins[name]
}

// selectorToPath converts a selector expression to a dotted path string
func selectorToPath(sel *ast.SelectorExpr) string {
	var parts []string

	// Get the final selector (Sel is ast.Label, use LabelName to extract)
	if sel.Sel != nil {
		name, _, _ := ast.LabelName(sel.Sel)
		if name != "" {
			parts = append(parts, name)
		}
	}

	// Walk up the selector chain
	current := sel.X
	for current != nil {
		switch x := current.(type) {
		case *ast.Ident:
			parts = append(parts, x.Name)
			current = nil
		case *ast.SelectorExpr:
			if x.Sel != nil {
				name, _, _ := ast.LabelName(x.Sel)
				if name != "" {
					parts = append(parts, name)
				}
			}
			current = x.X
		default:
			current = nil
		}
	}

	// Reverse to get correct order
	for i, j := 0, len(parts)-1; i < j; i, j = i+1, j-1 {
		parts[i], parts[j] = parts[j], parts[i]
	}

	return strings.Join(parts, ".")
}

// extractReferencesFromValue walks evaluated values to find reference paths.
// Uses CUE's ReferencePath() API which resolves through let bindings and aliases.
// This is schema-agnostic - it extracts reference paths for ALL values that have them.
func extractReferencesFromValue(v cue.Value, instancePath, fieldPath string, refs map[string]string) {
	walk := referenceWalk{instancePath: instancePath, refs: refs}
	walk.visit(v, fieldPath)
}

// referenceWalk records the reference path of every evaluated value.
type referenceWalk struct {
	instancePath string
	// source answers the fields whose declaration settles the reference;
	// empty when there is no source to consult.
	source sourceReferences
	refs   map[string]string
}

func (w *referenceWalk) visit(v cue.Value, fieldPath string) {
	// Skip invalid or error values
	if v.Err() != nil {
		return
	}

	// For every value, check if it came from a reference
	// Record the raw reference path - let consumers decide how to interpret it
	if fieldPath != "" {
		metaKey := makeMetaKey(w.instancePath, fieldPath)
		refPath, found := w.reference(v, metaKey)
		if refPath != "" || found != referenceAbsent {
			// An empty entry reserves an ambiguous field so the AST fallback
			// cannot choose one of several references arbitrarily.
			w.refs[metaKey] = refPath
		}
		if found == referenceThroughConstraint && refPath != "" && !reachesHiddenValue(refPath) {
			// A visible referenced value records its own fields where it is
			// declared. Walking them again through every constrained
			// reference re-evaluates a dependency chain once per level.
			return
		}
	}

	// Recurse into children
	switch v.Kind() {
	case cue.StructKind:
		// Use cue.Definitions(false) to exclude schema definitions (like #Task, #Project)
		// which can have recursive type hierarchies that cause hangs.
		// This matches buildValueClean() which also uses cue.Definitions(false).
		iter, _ := v.Fields(cue.Definitions(false))
		for iter.Next() {
			label := iter.Label()
			// Skip hidden fields (start with _) - they're internal to CUE
			if strings.HasPrefix(label, "_") {
				continue
			}
			childPath := label
			if fieldPath != "" {
				childPath = fieldPath + "." + label
			}
			w.visit(iter.Value(), childPath)
		}
	case cue.ListKind:
		list, _ := v.List()
		for i := 0; list.Next(); i++ {
			childPath := fmt.Sprintf("%s[%d]", fieldPath, i)
			w.visit(list.Value(), childPath)
		}
	}
}

// reference finds a value's reference path. The source answers it where it
// can, because Value.Expr on a constrained value re-evaluates the value and
// its whole dependency chain.
func (w *referenceWalk) reference(v cue.Value, metaKey string) (string, referenceFinding) {
	if direct := directReferencePath(v); direct != "" {
		return direct, referenceDirect
	}
	if declared, ok := w.source.declarations[metaKey]; ok {
		if declared == "" {
			return "", referenceAbsent
		}
		return declared, referenceThroughConstraint
	}
	return constrainedReferencePath(v)
}

// reachesHiddenValue reports whether a reference goes through a hidden field
// or a definition, whose fields the walk does not visit where they are
// declared.
func reachesHiddenValue(refPath string) bool {
	for _, segment := range strings.Split(refPath, ".") {
		if strings.HasPrefix(segment, "_") || strings.HasPrefix(segment, "#") {
			return true
		}
	}
	return false
}

// sourceReferences records what the source declares at each field and list
// element: the path of a bare reference (`build`, `tasks.build`), or empty
// for a value that cannot be a reference (a literal, or a definition
// conjoined with literals). Fields declared inconsistently, or as anything
// else, are left out for the evaluated value to answer.
type sourceReferences struct {
	declarations map[string]string
	conflicting  map[string]bool
}

func sourceReferencesFromAST(inst *build.Instance, instancePath string) sourceReferences {
	source := sourceReferences{
		declarations: make(map[string]string),
		conflicting:  make(map[string]bool),
	}
	for _, f := range inst.Files {
		for _, decl := range f.Decls {
			switch d := decl.(type) {
			case *ast.Field:
				label, _, _ := ast.LabelName(d.Label)
				source.walk(d.Value, makeMetaKey(instancePath, label), label, instancePath)
			case *ast.EmbedDecl:
				source.walk(d.Expr, "", "", instancePath)
			}
		}
	}
	return source
}

func (s *sourceReferences) walk(expr ast.Expr, metaKey, fieldPath, instancePath string) {
	if metaKey != "" {
		reference, known := declaredReference(expr)
		s.record(metaKey, reference, known)
		if reference != "" {
			return
		}
	}
	switch e := expr.(type) {
	case *ast.StructLit:
		for _, elem := range e.Elts {
			switch child := elem.(type) {
			case *ast.Field:
				label, _, _ := ast.LabelName(child.Label)
				childPath := label
				if fieldPath != "" {
					childPath = fieldPath + "." + label
				}
				s.walk(child.Value, makeMetaKey(instancePath, childPath), childPath, instancePath)
			case *ast.EmbedDecl:
				s.walk(child.Expr, "", fieldPath, instancePath)
			}
		}
	case *ast.ListLit:
		for i, elem := range e.Elts {
			indexPath := fmt.Sprintf("%s[%d]", fieldPath, i)
			s.walk(elem, makeMetaKey(instancePath, indexPath), indexPath, instancePath)
		}
	case *ast.BinaryExpr:
		// Reach the fields of the operands; the operands themselves share
		// this field's key.
		s.walk(e.X, "", fieldPath, instancePath)
		s.walk(e.Y, "", fieldPath, instancePath)
	case *ast.ParenExpr:
		s.walk(e.X, "", fieldPath, instancePath)
	}
}

// record keeps one declaration per key; a key declared twice differently,
// or once in a form the source cannot settle, is left to evaluation.
func (s *sourceReferences) record(metaKey, reference string, known bool) {
	if s.conflicting[metaKey] {
		return
	}
	existing, seen := s.declarations[metaKey]
	if known && (!seen || existing == reference) {
		s.declarations[metaKey] = reference
		return
	}
	delete(s.declarations, metaKey)
	s.conflicting[metaKey] = true
}

// declaredReference classifies a declared expression: the path it names when
// it is a bare reference, empty and known when it cannot be a reference, and
// unknown otherwise.
func declaredReference(expr ast.Expr) (string, bool) {
	switch e := expr.(type) {
	case *ast.Ident:
		if isBuiltinType(e.Name) || strings.HasPrefix(e.Name, "#") {
			return "", true
		}
		return e.Name, true
	case *ast.SelectorExpr:
		if _, ok := e.X.(*ast.Ident); !ok {
			return "", false
		}
		path := selectorToPath(e)
		if path == "" {
			return "", false
		}
		segments := strings.Split(path, ".")
		if strings.HasPrefix(segments[len(segments)-1], "#") {
			return "", true
		}
		return path, true
	case *ast.BasicLit, *ast.StructLit, *ast.ListLit, *ast.Interpolation:
		return "", true
	case *ast.ParenExpr:
		return declaredReference(e.X)
	case *ast.BinaryExpr:
		if e.Op != token.AND {
			return "", false
		}
		// Only a conjunction of values that are not references is settled
		// here; one with a reference operand is left to evaluation.
		for _, operand := range []ast.Expr{e.X, e.Y} {
			if reference, known := declaredReference(operand); !known || reference != "" {
				return "", false
			}
		}
		return "", true
	}
	return "", false
}

// safeReferencePath safely extracts the reference path from a CUE value.
// Returns empty string if the value is not a reference or if extraction fails.
func safeReferencePath(v cue.Value) string {
	result, _ := checkedReferencePath(v)
	return result
}

// referenceFinding says how a value's reference path was established.
type referenceFinding int

const (
	// referenceAbsent: the value is not a reference.
	referenceAbsent referenceFinding = iota
	// referenceDirect: the value is a plain reference.
	referenceDirect
	// referenceThroughConstraint: a schema constraint made the reference a
	// conjunction; its path is empty when the conjuncts disagree.
	referenceThroughConstraint
)

func checkedReferencePath(v cue.Value) (string, referenceFinding) {
	if direct := directReferencePath(v); direct != "" {
		return direct, referenceDirect
	}
	return constrainedReferencePath(v)
}

// directReferencePath is the path of a value that is a plain reference.
func directReferencePath(v cue.Value) (result string) {
	// Use recover to handle panics from ReferencePath on non-reference values
	defer func() {
		if r := recover(); r != nil {
			result = ""
		}
	}()
	root, path := v.ReferencePath()
	if root.Exists() {
		return path.String()
	}
	return ""
}

// constrainedReferencePath finds the reference a schema constraint turned
// into a conjunction. It accepts only one unambiguous value reference; type
// definitions and disjunctions do not identify the referenced value.
func constrainedReferencePath(v cue.Value) (result string, found referenceFinding) {
	defer func() {
		if r := recover(); r != nil {
			result = ""
			found = referenceThroughConstraint
		}
	}()
	reference, ambiguous := conjunctReferencePath(v)
	if reference == "" && !ambiguous {
		return "", referenceAbsent
	}
	return reference, referenceThroughConstraint
}

func conjunctReferencePath(v cue.Value) (string, bool) {
	op, args := v.Expr()
	if op != cue.AndOp {
		return "", false
	}
	var reference string
	for _, arg := range args {
		root, path := arg.ReferencePath()
		candidate := ""
		if root.Exists() {
			selectors := path.Selectors()
			if len(selectors) > 0 && selectors[len(selectors)-1].LabelType() != cue.DefinitionLabel {
				candidate = path.String()
			}
		} else if isConjunction(arg.Source()) {
			// Expand only a nested conjunction. Expr on any other conjunct
			// evaluates it afresh, repeating the dependency chain behind it
			// on every level, which grows exponentially with its depth.
			var ambiguous bool
			candidate, ambiguous = conjunctReferencePath(arg)
			if ambiguous {
				return "", true
			}
		}
		if candidate == "" {
			continue
		}
		if reference != "" && reference != candidate {
			return "", true
		}
		reference = candidate
	}
	return reference, false
}

// isConjunction reports whether a conjunct's source is itself a `&`
// expression, read from the syntax so nothing is evaluated.
func isConjunction(source ast.Node) bool {
	for {
		switch x := source.(type) {
		case *ast.ParenExpr:
			source = x.X
		case *ast.BinaryExpr:
			return x.Op == token.AND
		default:
			return false
		}
	}
}
