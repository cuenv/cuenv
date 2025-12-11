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

// buildJSONWithHidden builds a JSON representation of a CUE value including hidden fields.
// This is necessary because CUE's MarshalJSON() excludes hidden fields (prefixed with _).
// Hidden fields like _ci are package-scoped and don't unify across packages, making them
// useful for location-specific configuration that shouldn't be inherited.
func buildJSONWithHidden(v cue.Value, moduleRoot string, taskPositions map[string]TaskSourcePos) ([]byte, error) {
	result := make(map[string]interface{})

	iter, err := v.Fields(cue.Hidden(true))
	if err != nil {
		return nil, err
	}

	for iter.Next() {
		sel := iter.Selector()
		fieldName := sel.String()
		fieldValue := iter.Value()

		// Decode each field value to interface{}
		var val interface{}
		if err := fieldValue.Decode(&val); err != nil {
			return nil, fmt.Errorf("failed to decode field %s: %w", fieldName, err)
		}

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

//export cue_eval_package
func cue_eval_package(dirPath *C.char, packageName *C.char) *C.char {
	// Add recover to catch any panics
	var result *C.char
	defer func() {
		if r := recover(); r != nil {
			panic_msg := fmt.Sprintf("Internal panic: %v", r)
			result = createErrorResponse(ErrorCodePanicRecover, panic_msg, nil)
		}
	}()

	goDir := C.GoString(dirPath)
	goPackageName := C.GoString(packageName)

	// Validate inputs
	if goDir == "" {
		result = createErrorResponse(ErrorCodeInvalidInput, "Directory path cannot be empty", nil)
		return result
	}

	if goPackageName == "" {
		result = createErrorResponse(ErrorCodeInvalidInput, "Package name cannot be empty", nil)
		return result
	}

	// Create CUE context
	ctx := cuecontext.New()

	// Explicitly initialize the CUE module registry
	// This ensures proper access to the module cache and remote registry
	// Use the same configuration as the CUE CLI to ensure proper registry access
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

	// Ensure the CUE loader can resolve module-relative imports (e.g., schema)
	moduleRoot := resolveCueModuleRoot(goDir)

	// Create load configuration to load from specific directory
	cfg := &load.Config{
		Dir:      goDir,
		Registry: registry,
	}
	if moduleRoot != "" {
		cfg.ModuleRoot = moduleRoot
	}

	// Load the specific CUE package by name
	// This matches the behavior of "cue export .:package-name" but from a specific directory
	var instances []*build.Instance
	packagePath := ".:" + goPackageName
	instances = load.Instances([]string{packagePath}, cfg)

	if len(instances) == 0 {
		hint := "Check that the package name exists and CUE files are present"
		result = createErrorResponse(ErrorCodeLoadInstance, "No CUE instances found", &hint)
		return result
	}

	inst := instances[0]
	if inst.Err != nil {
		msg := fmt.Sprintf("Failed to load CUE instance: %v", inst.Err)
		hint := "Check CUE syntax and import statements"
		result = createErrorResponse(ErrorCodeLoadInstance, msg, &hint)
		return result
	}

	// Build the CUE value
	v := ctx.BuildInstance(inst)
	if v.Err() != nil {
		msg := fmt.Sprintf("Failed to build CUE value: %v", v.Err())
		hint := "Check CUE constraints and value definitions"
		result = createErrorResponse(ErrorCodeBuildValue, msg, &hint)
		return result
	}

	// Extract task positions from AST (CUE's Pos() returns schema positions after unification)
	taskPositions := extractTaskPositions(inst, moduleRoot)

	// Build JSON including hidden fields (like _ci) which are package-scoped
	// and don't participate in cross-package unification
	// Pass taskPositions so source metadata uses actual definition locations
	jsonBytes, err := buildJSONWithHidden(v, moduleRoot, taskPositions)
	if err != nil {
		msg := fmt.Sprintf("Failed to marshal JSON: %v", err)
		result = createErrorResponse(ErrorCodeOrderedJSON, msg, nil)
		return result
	}

	// Return success response with JSON data
	result = createSuccessResponse(string(jsonBytes))
	return result
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
