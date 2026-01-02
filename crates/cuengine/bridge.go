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

// TaskSourcePos holds the source position of a task definition
type TaskSourcePos struct {
	File   string
	Line   int
	Column int
}

// ValueMeta holds source location metadata for a concrete value
type ValueMeta struct {
	Directory string `json:"directory"`
	Filename  string `json:"filename"`
	Line      int    `json:"line"`
}

// MetaValue wraps a concrete value with its source metadata
type MetaValue struct {
	Value interface{} `json:"_value"`
	Meta  ValueMeta   `json:"_meta"`
}

// extractTaskPositions walks the AST files to find where each task is defined.
// CUE's evaluated Pos() returns schema positions, but AST gives actual definition locations.
func extractTaskPositions(inst *build.Instance, moduleRoot string) map[string]TaskSourcePos {
	positions := make(map[string]TaskSourcePos)

	for _, f := range inst.Files {
		for _, decl := range f.Decls {
			field, ok := decl.(*ast.Field)
			if !ok {
				continue
			}

			label, _, _ := ast.LabelName(field.Label)
			if label != "tasks" {
				continue
			}

			// Found tasks field, extract nested task positions
			st, ok := field.Value.(*ast.StructLit)
			if !ok {
				continue
			}

			extractTaskPositionsFromStruct(st, "", f.Filename, moduleRoot, positions)
		}
	}

	return positions
}

// extractTaskPositionsFromStruct recursively extracts task positions from a struct literal
func extractTaskPositionsFromStruct(st *ast.StructLit, prefix, filename, moduleRoot string, positions map[string]TaskSourcePos) {
	for _, elem := range st.Elts {
		taskField, ok := elem.(*ast.Field)
		if !ok {
			continue
		}

		taskLabel, _, _ := ast.LabelName(taskField.Label)
		fullName := taskLabel
		if prefix != "" {
			fullName = prefix + "." + taskLabel
		}

		// Make path relative to moduleRoot
		relPath := filename
		if moduleRoot != "" && strings.HasPrefix(filename, moduleRoot) {
			relPath = strings.TrimPrefix(filename, moduleRoot)
			relPath = strings.TrimPrefix(relPath, string(filepath.Separator))
		}
		if relPath == "" {
			relPath = "env.cue"
		}

		pos := taskField.Pos()
		positions[fullName] = TaskSourcePos{
			File:   relPath,
			Line:   pos.Line(),
			Column: pos.Column(),
		}

		// Check for nested tasks (parallel groups have a "tasks" field)
		if nestedSt, ok := taskField.Value.(*ast.StructLit); ok {
			for _, nestedElem := range nestedSt.Elts {
				if nestedField, ok := nestedElem.(*ast.Field); ok {
					nestedLabel, _, _ := ast.LabelName(nestedField.Label)
					if nestedLabel == "tasks" {
						// This is a parallel group, recurse into its tasks
						if nestedTasksSt, ok := nestedField.Value.(*ast.StructLit); ok {
							extractTaskPositionsFromStruct(nestedTasksSt, fullName, filename, moduleRoot, positions)
						}
					}
				}
			}
		}
	}
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

// buildJSONWithMeta builds a JSON representation with _meta attached to all concrete values
func buildJSONWithMeta(v cue.Value, positions map[string]ValueMeta) ([]byte, error) {
	result := buildValueWithMeta(v, "", positions)
	return json.Marshal(result)
}

// buildValueWithMeta recursively builds a value with _meta annotations
func buildValueWithMeta(v cue.Value, path string, positions map[string]ValueMeta) interface{} {
	// Check the kind of value
	switch v.Kind() {
	case cue.StructKind:
		result := make(map[string]interface{})
		iter, _ := v.Fields(cue.Definitions(false))
		for iter.Next() {
			sel := iter.Selector()
			fieldName := unquoteSelector(sel.String())
			childPath := fieldName
			if path != "" {
				childPath = path + "." + fieldName
			}
			result[fieldName] = buildValueWithMeta(iter.Value(), childPath, positions)
		}
		return result

	case cue.ListKind:
		// For lists, check if we have position info for the list itself
		var items []interface{}
		iter, _ := v.List()
		i := 0
		for iter.Next() {
			indexPath := fmt.Sprintf("%s[%d]", path, i)
			items = append(items, buildValueWithMeta(iter.Value(), indexPath, positions))
			i++
		}
		// Wrap the list with _meta if we have position info
		if meta, ok := positions[path]; ok {
			return MetaValue{Value: items, Meta: meta}
		}
		return items

	default:
		// Concrete value (string, number, bool, null)
		var val interface{}
		v.Decode(&val)

		// Look up position from AST
		if meta, ok := positions[path]; ok {
			return MetaValue{Value: val, Meta: meta}
		}
		return val
	}
}

// buildJSON builds a JSON representation of a CUE value.
// This excludes hidden fields (_foo) and definitions (#Foo) which are
// internal CUE constructs not meant for export.
func buildJSON(v cue.Value, moduleRoot string, taskPositions map[string]TaskSourcePos) ([]byte, error) {
	result := make(map[string]interface{})

	iter, err := v.Fields(cue.Definitions(false))
	if err != nil {
		return nil, err
	}

	for iter.Next() {
		sel := iter.Selector()
		fieldName := unquoteSelector(sel.String())
		fieldValue := iter.Value()

		// Build value recursively, excluding definitions at every level
		val := buildValueClean(fieldValue)

		// For tasks field, enrich with source position metadata from AST
		if fieldName == "tasks" {
			val = enrichTasksWithSource(val, "", taskPositions)
		}

		result[fieldName] = val
	}

	return json.Marshal(result)
}

// enrichTasksWithSource adds _source metadata to each task in the tasks map using AST positions
func enrichTasksWithSource(decoded interface{}, prefix string, positions map[string]TaskSourcePos) interface{} {
	tasksMap, ok := decoded.(map[string]interface{})
	if !ok {
		return decoded
	}

	for taskName, taskDef := range tasksMap {
		fullName := taskName
		if prefix != "" {
			fullName = prefix + "." + taskName
		}
		enrichTaskWithSource(taskDef, fullName, positions)
	}
	return tasksMap
}

// enrichTaskWithSource adds _source metadata to a single task definition using AST positions
// Only adds to leaf tasks (those with command/script), not to group definitions
func enrichTaskWithSource(taskDef interface{}, fullName string, positions map[string]TaskSourcePos) {
	taskObj, ok := taskDef.(map[string]interface{})
	if !ok {
		return
	}

	// Check if this is a task group (has "tasks" field) - if so, only recurse, don't add _source
	if nested, ok := taskObj["tasks"].(map[string]interface{}); ok {
		// This is a parallel group - recurse into children
		for childName, childDef := range nested {
			childFullName := fullName + "." + childName
			enrichTaskWithSource(childDef, childFullName, positions)
		}
		return
	}

	// Check if this is a sequential group (is an array)
	if _, isArray := taskDef.([]interface{}); isArray {
		// Sequential groups are arrays, skip adding _source
		return
	}

	// This is a leaf task (has command or script) - add _source metadata
	_, hasCommand := taskObj["command"]
	_, hasScript := taskObj["script"]
	_, hasTaskRef := taskObj["task_ref"]

	if !hasCommand && !hasScript && !hasTaskRef {
		// Not a valid leaf task, skip
		return
	}

	// Look up position from AST-extracted map
	if pos, ok := positions[fullName]; ok {
		taskObj["_source"] = map[string]interface{}{
			"file":   pos.File,
			"line":   pos.Line,
			"column": pos.Column,
		}
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
	WithMeta    bool    `json:"withMeta"`    // Extract source positions into separate Meta map
	Recursive   bool    `json:"recursive"`   // true: cue eval ./..., false: cue eval .
	PackageName *string `json:"packageName"` // Filter to specific package, nil = all packages
	TargetDir   *string `json:"targetDir"`   // Directory to evaluate (for non-recursive), nil = module root
}

// instanceResult holds the result of evaluating a single CUE instance (used for parallel evaluation)
type instanceResult struct {
	relPath   string
	jsonBytes []byte
	isProject bool
	meta      map[string]ValueMeta
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

			results <- instanceResult{
				relPath:   b.relPath,
				jsonBytes: jsonBytes,
				isProject: b.isProject,
				meta:      meta,
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
	if options.WithMeta && len(allMeta) > 0 {
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

// findEnvCueDirectories walks the module root and finds all directories containing env.cue
// files that declare the specified package
func findEnvCueDirectories(moduleRoot, packageName string) ([]string, error) {
	var dirs []string

	err := filepath.Walk(moduleRoot, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return nil // Skip directories we can't access
		}

		// Skip hidden directories and cue.mod
		if info.IsDir() {
			name := info.Name()
			if strings.HasPrefix(name, ".") || name == "cue.mod" || name == "node_modules" {
				return filepath.SkipDir
			}
			return nil
		}

		// Check for env.cue files
		if info.Name() == "env.cue" {
			// Verify it has the right package declaration
			content, err := os.ReadFile(path)
			if err != nil {
				return nil
			}

			// Simple package detection - look for "package <name>"
			if containsPackageDecl(string(content), packageName) {
				dirs = append(dirs, filepath.Dir(path))
			}
		}

		return nil
	})

	return dirs, err
}

// containsPackageDecl checks if file content declares the given package
func containsPackageDecl(content, packageName string) bool {
	lines := strings.Split(content, "\n")
	for _, line := range lines {
		line = strings.TrimSpace(line)
		// Skip comments
		if strings.HasPrefix(line, "//") {
			continue
		}
		// Check for package declaration
		if strings.HasPrefix(line, "package ") {
			parts := strings.Fields(line)
			if len(parts) >= 2 && parts[1] == packageName {
				return true
			}
		}
		// Stop after we've passed the package declaration area
		if strings.HasPrefix(line, "import") || strings.Contains(line, "{") {
			break
		}
	}
	return false
}

// resolveCueModuleRoot attempts to find the nearest cue.mod root so imports work from nested directories
func resolveCueModuleRoot(startDir string) string {
	// Environment override takes precedence for packaged binaries
	if envRoot := os.Getenv("CUENV_CUE_MODULE_ROOT"); envRoot != "" {
		if info, err := os.Stat(filepath.Join(envRoot, "cue.mod", "module.cue")); err == nil && !info.IsDir() {
			return envRoot
		}
	}

	dir, err := filepath.Abs(startDir)
	if err != nil {
		dir = startDir
	}

	for {
		moduleFile := filepath.Join(dir, "cue.mod", "module.cue")
		if info, err := os.Stat(moduleFile); err == nil && !info.IsDir() {
			return dir
		}

		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}

	return ""
}

func main() {}
