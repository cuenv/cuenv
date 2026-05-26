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

// MetaValue wraps a concrete value with its source metadata
type MetaValue struct {
	Value interface{} `json:"_value"`
	Meta  ValueMeta   `json:"_meta"`
}

// extractAllFieldPositions walks the AST to extract source positions for ALL fields.
// Returns a map of dotted path -> ValueMeta (e.g., "owners.rules.**" -> {dir, file, line})
func extractAllFieldPositions(inst *build.Instance, moduleRoot, instanceDir string) map[string]ValueMeta {
	positions := make(map[string]ValueMeta)

	for _, f := range inst.Files {
		// Calculate relative path from moduleRoot for the filename
		relPath := f.Filename
		if moduleRoot != "" && strings.HasPrefix(f.Filename, moduleRoot) {
			relPath = strings.TrimPrefix(f.Filename, moduleRoot)
			relPath = strings.TrimPrefix(relPath, string(filepath.Separator))
		}
		if relPath == "" {
			relPath = "env.cue"
		}

		// Calculate the directory relative to moduleRoot
		dir := instanceDir
		if dir == "" || dir == "." {
			dir = "."
		}

		for _, decl := range f.Decls {
			field, ok := decl.(*ast.Field)
			if !ok {
				continue
			}

			label, _, _ := ast.LabelName(field.Label)
			extractFieldPositionsRecursive(field, label, relPath, dir, positions)
		}
	}

	return positions
}

// extractFieldPositionsRecursive recursively extracts positions for a field and its children
func extractFieldPositionsRecursive(field *ast.Field, path, filename, directory string, positions map[string]ValueMeta) {
	pos := field.Pos()
	positions[path] = ValueMeta{
		Directory: directory,
		Filename:  filename,
		Line:      pos.Line(),
	}

	// Recurse into struct literals
	if st, ok := field.Value.(*ast.StructLit); ok {
		for _, elem := range st.Elts {
			if childField, ok := elem.(*ast.Field); ok {
				childLabel, _, _ := ast.LabelName(childField.Label)
				childPath := path + "." + childLabel
				extractFieldPositionsRecursive(childField, childPath, filename, directory, positions)
			}
		}
	}

	// Recurse into list literals (arrays)
	if list, ok := field.Value.(*ast.ListLit); ok {
		for i, elem := range list.Elts {
			// Handle struct elements within lists
			if st, ok := elem.(*ast.StructLit); ok {
				indexPath := fmt.Sprintf("%s[%d]", path, i)
				for _, structElem := range st.Elts {
					if childField, ok := structElem.(*ast.Field); ok {
						childLabel, _, _ := ast.LabelName(childField.Label)
						childPath := indexPath + "." + childLabel
						extractFieldPositionsRecursive(childField, childPath, filename, directory, positions)
					}
				}
			}
		}
	}
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

		// Calculate the directory relative to moduleRoot
		dir := instancePath
		if dir == "" {
			dir = "."
		}

		for _, decl := range f.Decls {
			field, ok := decl.(*ast.Field)
			if !ok {
				continue
			}

			label, _, _ := ast.LabelName(field.Label)
			extractFieldMetaRecursive(field, label, relPath, dir, instancePath, positions)
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

	// Recurse into struct literals
	if st, ok := field.Value.(*ast.StructLit); ok {
		for _, elem := range st.Elts {
			if childField, ok := elem.(*ast.Field); ok {
				childLabel, _, _ := ast.LabelName(childField.Label)
				childPath := fieldPath + "." + childLabel
				extractFieldMetaRecursive(childField, childPath, filename, directory, instancePath, positions)
			}
		}
	}

	// Recurse into list literals (arrays)
	if list, ok := field.Value.(*ast.ListLit); ok {
		for i, elem := range list.Elts {
			// Handle struct elements within lists
			if st, ok := elem.(*ast.StructLit); ok {
				indexPath := fmt.Sprintf("%s[%d]", fieldPath, i)
				for _, structElem := range st.Elts {
					if childField, ok := structElem.(*ast.Field); ok {
						childLabel, _, _ := ast.LabelName(childField.Label)
						childPath := indexPath + "." + childLabel
						extractFieldMetaRecursive(childField, childPath, filename, directory, instancePath, positions)
					}
				}
			}
		}
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
	// Skip invalid or error values
	if v.Err() != nil {
		return
	}

	// For every value, check if it came from a reference
	// Record the raw reference path - let consumers decide how to interpret it
	if fieldPath != "" {
		if refPath := safeReferencePath(v); refPath != "" {
			metaKey := fmt.Sprintf("%s/%s", instancePath, fieldPath)
			refs[metaKey] = refPath
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
			extractReferencesFromValue(iter.Value(), instancePath, childPath, refs)
		}
	case cue.ListKind:
		list, _ := v.List()
		for i := 0; list.Next(); i++ {
			childPath := fmt.Sprintf("%s[%d]", fieldPath, i)
			extractReferencesFromValue(list.Value(), instancePath, childPath, refs)
		}
	}
}

// safeReferencePath safely extracts the reference path from a CUE value.
// Returns empty string if the value is not a reference or if extraction fails.
func safeReferencePath(v cue.Value) (result string) {
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
