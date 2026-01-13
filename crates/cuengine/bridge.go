package main

/*
#include <stdlib.h>
*/
import "C"
import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"unsafe"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/load"
	"cuelang.org/go/mod/modconfig"
)

const BridgeVersion = "bridge/1"

// Bridge error codes - keep in sync with Rust side
const (
	ErrorCodeInvalidInput  = "INVALID_INPUT"
	ErrorCodeLoadInstance  = "LOAD_INSTANCE"
	ErrorCodeBuildValue    = "BUILD_VALUE"
	ErrorCodeOrderedJSON   = "ORDERED_JSON"
	ErrorCodePanicRecover  = "PANIC_RECOVER"
	ErrorCodeJSONMarshal   = "JSON_MARSHAL_ERROR"
	ErrorCodeRegistryInit  = "REGISTRY_INIT"
	ErrorCodeDependencyRes = "DEPENDENCY_RESOLUTION"
)

// BridgeError represents an error in the bridge response
type BridgeError struct {
	Code    string  `json:"code"`
	Message string  `json:"message"`
	Hint    *string `json:"hint,omitempty"`
}

// BridgeResponse represents the structured response envelope
type BridgeResponse struct {
	Version string           `json:"version"`
	Ok      *json.RawMessage `json:"ok,omitempty"`
	Error   *BridgeError     `json:"error,omitempty"`
}

//export cue_free_string
func cue_free_string(s *C.char) {
	C.free(unsafe.Pointer(s))
}

//export cue_bridge_version
func cue_bridge_version() *C.char {
	versionInfo := fmt.Sprintf("%s (Go %s)", BridgeVersion, runtime.Version())
	return C.CString(versionInfo)
}

// Helper function to create error response
func createErrorResponse(code, message string, hint *string) *C.char {
	error := &BridgeError{
		Code:    code,
		Message: message,
		Hint:    hint,
	}
	response := &BridgeResponse{
		Version: BridgeVersion,
		Error:   error,
	}
	responseBytes, err := json.Marshal(response)
	if err != nil {
		// Fallback error response if JSON marshaling fails
		fallbackResponse := fmt.Sprintf(`{"version":"%s","error":{"code":"%s","message":"Failed to marshal error response: %s"}}`, BridgeVersion, ErrorCodeJSONMarshal, err.Error())
		return C.CString(fallbackResponse)
	}
	return C.CString(string(responseBytes))
}

// Helper function to create success response
func createSuccessResponse(data string) *C.char {
	// Convert string to RawMessage to preserve field ordering
	rawData := json.RawMessage(data)
	response := &BridgeResponse{
		Version: BridgeVersion,
		Ok:      &rawData,
	}
	responseBytes, err := json.Marshal(response)
	if err != nil {
		// If success response marshaling fails, return error response instead
		msg := fmt.Sprintf("Failed to marshal success response: %s", err.Error())
		return createErrorResponse(ErrorCodeJSONMarshal, msg, nil)
	}
	return C.CString(string(responseBytes))
}

// ValueMeta holds source location metadata for a concrete value
type ValueMeta struct {
	Directory string `json:"directory"`
	Filename  string `json:"filename"`
	Line      int    `json:"line"`
	Reference string `json:"reference,omitempty"` // If this value is a reference, the path it refers to
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

// buildJSONClean builds a JSON representation without any _meta injection.
// This returns clean JSON that can be correlated with the separate meta map.
func buildJSONClean(v cue.Value) ([]byte, error) {
	result := buildValueClean(v)
	return json.Marshal(result)
}

// unquoteSelector strips surrounding quotes from a selector string.
// CUE's Selector.String() returns quoted strings for string-keyed fields,
// e.g., `"test.json"` instead of `test.json`. We need the unquoted form
// for proper JSON serialization and file path handling.
func unquoteSelector(s string) string {
	if len(s) >= 2 && s[0] == '"' && s[len(s)-1] == '"' {
		return s[1 : len(s)-1]
	}
	return s
}

// buildValueClean recursively builds a clean value without metadata
func buildValueClean(v cue.Value) interface{} {
	switch v.Kind() {
	case cue.StructKind:
		result := make(map[string]interface{})
		iter, _ := v.Fields(cue.Definitions(false))
		for iter.Next() {
			sel := iter.Selector()
			fieldName := unquoteSelector(sel.String())
			result[fieldName] = buildValueClean(iter.Value())
		}
		return result

	case cue.ListKind:
		var items []interface{}
		iter, _ := v.List()
		for iter.Next() {
			items = append(items, buildValueClean(iter.Value()))
		}
		return items

	default:
		// Concrete value (string, number, bool, null)
		var val interface{}
		v.Decode(&val)
		return val
	}
}

// ModuleInstance represents a single evaluated CUE instance within a module
type ModuleInstance struct {
	Path  string          `json:"path"`
	Value json.RawMessage `json:"value"`
}

// ModuleResult contains all evaluated instances in a module
type ModuleResult struct {
	Instances map[string]json.RawMessage `json:"instances"`
	Projects  []string                   `json:"projects"`       // paths that conform to schema.#Project
	Meta      map[string]ValueMeta       `json:"meta,omitempty"` // "path/field" -> source location
}

// ModuleEvalOptions controls how module evaluation behaves
type ModuleEvalOptions struct {
	WithMeta       bool    `json:"withMeta"`       // Extract source positions into separate Meta map
	WithReferences bool    `json:"withReferences"` // Extract reference paths (requires WithMeta)
	Recursive      bool    `json:"recursive"`      // true: cue eval ./..., false: cue eval .
	PackageName    *string `json:"packageName"`    // Filter to specific package, nil = all packages
	TargetDir      *string `json:"targetDir"`      // Directory to evaluate (for non-recursive), nil = module root
}

// instanceResult holds the result of evaluating a single CUE instance (used for parallel evaluation)
type instanceResult struct {
	relPath   string
	jsonBytes []byte
	isProject bool
	meta      map[string]ValueMeta
	refs      map[string]string // Reference paths extracted from cue.Value
	err       error
}

//export cue_eval_module
func cue_eval_module(moduleRootPath *C.char, packageName *C.char, optionsJSON *C.char) *C.char {
	// Add recover to catch any panics
	var result *C.char
	defer func() {
		if r := recover(); r != nil {
			panic_msg := fmt.Sprintf("Internal panic: %v", r)
			result = createErrorResponse(ErrorCodePanicRecover, panic_msg, nil)
		}
	}()

	goModuleRoot := C.GoString(moduleRootPath)
	goPackageName := C.GoString(packageName) // Legacy parameter for backwards compatibility
	goOptionsJSON := C.GoString(optionsJSON)

	// Parse options (with defaults)
	options := ModuleEvalOptions{
		WithMeta:  false,
		Recursive: false,
	}
	if goOptionsJSON != "" {
		if err := json.Unmarshal([]byte(goOptionsJSON), &options); err != nil {
			hint := "Options must be valid JSON: {\"withMeta\": true, \"recursive\": true, \"packageName\": \"pkg\"}"
			result = createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Failed to parse options: %v", err), &hint)
			return result
		}
	}

	// PackageName from options takes precedence over legacy parameter
	effectivePackageName := goPackageName
	if options.PackageName != nil {
		effectivePackageName = *options.PackageName
	}

	// Validate inputs
	if goModuleRoot == "" {
		result = createErrorResponse(ErrorCodeInvalidInput, "Module root path cannot be empty", nil)
		return result
	}

	// Verify module root exists
	moduleFile := filepath.Join(goModuleRoot, "cue.mod", "module.cue")
	if _, err := os.Stat(moduleFile); os.IsNotExist(err) {
		hint := "Ensure path contains a cue.mod/module.cue file"
		result = createErrorResponse(ErrorCodeInvalidInput, "Not a valid CUE module root", &hint)
		return result
	}

	// NOTE: We don't create a global CUE context here because each goroutine
	// creates its own context for thread safety during parallel evaluation.

	// Initialize registry
	registry, err := modconfig.NewRegistry(&modconfig.Config{
		Transport:  http.DefaultTransport,
		ClientType: "cuenv",
	})
	if err != nil {
		hint := "Check CUE registry configuration (CUE_REGISTRY env var) and network access"
		result = createErrorResponse(ErrorCodeRegistryInit,
			fmt.Sprintf("Failed to initialize CUE registry: %v", err), &hint)
		return result
	}

	// Configure load pattern based on recursive option
	// recursive: true  -> cue eval ./...
	// recursive: false -> cue eval .
	//
	// For non-recursive evaluation, TargetDir specifies which directory to evaluate.
	// This allows evaluating a subdirectory while still using the module root for imports.
	evalDir := goModuleRoot
	if options.TargetDir != nil && *options.TargetDir != "" {
		evalDir = *options.TargetDir
	}

	cfg := &load.Config{
		Dir:        evalDir,
		ModuleRoot: goModuleRoot,
		Registry:   registry,
	}

	var loadPattern string
	if options.Recursive {
		loadPattern = "./..."
	} else {
		loadPattern = "."
	}

	// NOTE: We intentionally do NOT append ":packageName" to the load pattern.
	// Using "./...:cuenv" causes CUE to create instances for EVERY directory
	// by unifying ancestor package files, not just directories with .cue files.
	// Instead, we filter by package name in post-processing below.

	// Load CUE instances using native CUE loader
	loadedInstances := load.Instances([]string{loadPattern}, cfg)
	if len(loadedInstances) == 0 {
		hint := "No CUE files found matching the load pattern"
		result = createErrorResponse(ErrorCodeLoadInstance, "No CUE instances found", &hint)
		return result
	}

	// NOTE: We don't load the schema package separately anymore.
	// The schema is already imported by each CUE file (import "github.com/cuenv/cuenv/schema")
	// and validated during BuildInstance. We detect Projects by checking for the required
	// "name" field (Projects have name!, Bases don't) instead of expensive schema unification.

	// Pre-filter valid instances (cheap filtering before parallelization)
	var validInstances []*build.Instance
	var loadErrors []string
	var packageMismatches []string
	for _, inst := range loadedInstances {
		if inst.Err != nil {
			loadErrors = append(loadErrors, fmt.Sprintf("%s: %v", inst.Dir, inst.Err))
			continue
		}
		if effectivePackageName != "" && inst.PkgName != effectivePackageName {
			packageMismatches = append(packageMismatches, fmt.Sprintf("%s has package '%s'", inst.Dir, inst.PkgName))
			continue
		}
		validInstances = append(validInstances, inst)
	}

	// Prepare result containers
	instances := make(map[string]json.RawMessage)
	projects := []string{} // Use empty slice, not nil, so JSON serializes as [] instead of null
	allMeta := make(map[string]ValueMeta)
	var buildErrors []string

	// Build CUE values SEQUENTIALLY to avoid race conditions.
	// CUE's build.Instance objects share internal state (file caches, parsed ASTs),
	// so concurrent BuildInstance calls on different instances can race.
	// JSON serialization is expensive but thread-safe, so we parallelize that below.
	type builtInstance struct {
		relPath   string
		value     cue.Value
		isProject bool
		inst      *build.Instance // Needed for meta extraction
	}
	var builtInstances []builtInstance

	ctx := cuecontext.New()
	for _, inst := range validInstances {
		// Calculate relative path from module root
		relPath, err := filepath.Rel(goModuleRoot, inst.Dir)
		if err != nil {
			relPath = inst.Dir
		}
		if relPath == "" {
			relPath = "."
		}

		// Build the CUE value (must be sequential)
		v := ctx.BuildInstance(inst)
		if v.Err() != nil {
			// Collect build errors so they can be reported if no instances succeed
			buildErrors = append(buildErrors, fmt.Sprintf("%s: %v", relPath, v.Err()))
			continue
		}

		// Check if this is a Project (has required "name" field) vs Base (no name)
		isProject := false
		nameField := v.LookupPath(cue.ParsePath("name"))
		if nameField.Exists() && nameField.Err() == nil {
			isProject = true
		}

		builtInstances = append(builtInstances, builtInstance{
			relPath:   relPath,
			value:     v,
			isProject: isProject,
			inst:      inst,
		})
	}

	// Parallel JSON serialization with worker pool
	// JSON marshaling is expensive but thread-safe, so we parallelize this part.
	results := make(chan instanceResult, len(builtInstances))
	var wg sync.WaitGroup

	// Limit concurrency to avoid memory pressure
	semaphore := make(chan struct{}, runtime.NumCPU())

	// Capture variables for goroutines
	moduleRoot := goModuleRoot
	withMeta := options.WithMeta
	withReferences := options.WithReferences

	for _, built := range builtInstances {
		wg.Add(1)
		go func(b builtInstance) {
			defer wg.Done()
			semaphore <- struct{}{}        // Acquire
			defer func() { <-semaphore }() // Release

			// Build clean JSON (without inline _meta) - thread-safe
			jsonBytes, err := buildJSONClean(b.value)
			if err != nil {
				results <- instanceResult{err: err}
				return
			}

			// Extract meta separately if requested - thread-safe (read-only)
			var meta map[string]ValueMeta
			if withMeta {
				meta = extractFieldMetaSeparate(b.inst, moduleRoot, b.relPath)
			}

			// Extract reference paths if requested - thread-safe (read-only)
			var refs map[string]string
			if withReferences {
				refs = make(map[string]string)
				// Extract from evaluated value for canonical paths (resolves let bindings)
				extractReferencesFromValue(b.value, b.relPath, "", refs)
				// Fall back to AST extraction for other references (backwards compat)
				astRefs := extractReferencesFromAST(b.inst, b.relPath)
				for k, v := range astRefs {
					if _, exists := refs[k]; !exists {
						refs[k] = v
					}
				}
			}

			results <- instanceResult{
				relPath:   b.relPath,
				jsonBytes: jsonBytes,
				isProject: b.isProject,
				meta:      meta,
				refs:      refs,
			}
		}(built)
	}

	// Close results channel when all goroutines complete
	go func() {
		wg.Wait()
		close(results)
	}()

	// Collect results (order doesn't matter for maps)
	for r := range results {
		if r.err != nil {
			buildErrors = append(buildErrors, r.err.Error())
			continue // Skip failed instances
		}
		instances[r.relPath] = json.RawMessage(r.jsonBytes)
		if r.isProject {
			projects = append(projects, r.relPath)
		}
		for k, v := range r.meta {
			allMeta[k] = v
		}
		// Merge reference paths into meta entries
		for k, refPath := range r.refs {
			if existing, ok := allMeta[k]; ok {
				existing.Reference = refPath
				allMeta[k] = existing
			} else {
				// Create a meta entry with just the reference if no source position exists
				allMeta[k] = ValueMeta{Reference: refPath}
			}
		}
	}

	if len(instances) == 0 {
		allErrors := append(loadErrors, buildErrors...)
		hint := fmt.Sprintf("evalDir=%s, moduleRoot=%s, loadPattern=%s, package=%s, loadedInstances=%d, validInstances=%d, builtInstances=%d, errors=%v, packageMismatches=%v",
			evalDir, goModuleRoot, loadPattern, effectivePackageName, len(loadedInstances), len(validInstances), len(builtInstances), allErrors, packageMismatches)
		result = createErrorResponse(ErrorCodeBuildValue, "No instances could be evaluated", &hint)
		return result
	}

	// Marshal the result
	moduleResult := ModuleResult{
		Instances: instances,
		Projects:  projects,
	}
	if (options.WithMeta || options.WithReferences) && len(allMeta) > 0 {
		moduleResult.Meta = allMeta
	}

	resultBytes, err := json.Marshal(moduleResult)
	if err != nil {
		result = createErrorResponse(ErrorCodeJSONMarshal, fmt.Sprintf("Failed to marshal module result: %v", err), nil)
		return result
	}

	result = createSuccessResponse(string(resultBytes))
	return result
}

func main() {}
