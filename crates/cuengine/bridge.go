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
	"runtime/debug"
	"strconv"
	"strings"
	"unsafe"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
	cueerrors "cuelang.org/go/cue/errors"
	"cuelang.org/go/cue/load"
	cueparser "cuelang.org/go/cue/parser"
	"cuelang.org/go/mod/modconfig"
	"cuelang.org/go/mod/modfile"
)

const BridgeVersion = "bridge/1"

// init relaxes the Go garbage collector for the embedded CUE evaluator.
//
// CUE evaluation is allocation-heavy and short-lived: with the default
// GOGC=100 roughly half of the evaluation CPU time is spent in GC
// (runtime.scanobject/findObject). Raising the GC target trades transient
// memory for a substantially faster evaluation. Users can still override the
// behavior by setting GOGC explicitly.
func init() {
	if os.Getenv("GOGC") == "" {
		debug.SetGCPercent(800)
	}
}

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

type moduleDependencyVersion struct {
	Version *string `json:"version"`
}

func readModuleFile(moduleRoot string) (string, []byte, error) {
	if moduleRoot == "" {
		return "", nil, fmt.Errorf("module root path cannot be empty")
	}
	moduleFile := filepath.Join(moduleRoot, "cue.mod", "module.cue")
	data, err := os.ReadFile(moduleFile)
	if err != nil {
		return moduleFile, nil, fmt.Errorf("failed to read %s: %w", moduleFile, err)
	}
	return moduleFile, data, nil
}

func parseModuleFile(moduleRoot string) (*modfile.File, string, error) {
	moduleFile, data, err := readModuleFile(moduleRoot)
	if err != nil {
		return nil, moduleFile, err
	}
	file, err := modfile.ParseNonStrict(data, moduleFile)
	if err != nil {
		return nil, moduleFile, err
	}
	return file, moduleFile, nil
}

//export cue_module_dependency_version
func cue_module_dependency_version(moduleRootPath *C.char, dependencyPath *C.char) *C.char {
	var result *C.char
	defer func() {
		if r := recover(); r != nil {
			panicMsg := fmt.Sprintf("Internal panic: %v", r)
			result = createErrorResponse(ErrorCodePanicRecover, panicMsg, nil)
		}
	}()

	moduleRoot := C.GoString(moduleRootPath)
	dependencyBasePath := C.GoString(dependencyPath)
	file, moduleFile, err := parseModuleFile(moduleRoot)
	if err != nil {
		hint := "Ensure path contains a valid cue.mod/module.cue file"
		result = createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Failed to parse %s: %v", moduleFile, err), &hint)
		return result
	}

	var version *string
	if file.Deps != nil {
		for depPath, dep := range file.Deps {
			if dep == nil || moduleBasePath(depPath) != dependencyBasePath {
				continue
			}
			rawVersion := dep.Version
			version = &rawVersion
			break
		}
	}

	payload, err := json.Marshal(moduleDependencyVersion{Version: version})
	if err != nil {
		result = createErrorResponse(ErrorCodeJSONMarshal, fmt.Sprintf("Failed to marshal module dependency version: %v", err), nil)
		return result
	}
	result = createSuccessResponse(string(payload))
	return result
}

func moduleBasePath(path string) string {
	basePath, _, found := strings.Cut(path, "@v")
	if !found {
		return path
	}
	return basePath
}

// ModuleInstance represents a single evaluated CUE instance within a module
type ModuleInstance struct {
	Path  string          `json:"path"`
	Value json.RawMessage `json:"value"`
}

// ModuleResult contains every successfully evaluated selected-package instance.
// The bridge returns no ModuleResult if any selected instance fails.
type ModuleResult struct {
	Instances map[string]json.RawMessage `json:"instances"`
	Projects  []string                   `json:"projects"`       // paths whose serialized JSON contains a concrete string name
	Meta      map[string]ValueMeta       `json:"meta,omitempty"` // "path/field" -> source location
}

// ModuleEvalOptions controls how module evaluation behaves
type ModuleEvalOptions struct {
	WithMeta       bool    `json:"withMeta"`       // Extract source positions into separate Meta map
	WithReferences bool    `json:"withReferences"` // Extract reference paths (requires WithMeta)
	Recursive      bool    `json:"recursive"`      // true: cue eval ./..., false: cue eval .
	PackageName    *string `json:"packageName"`    // Overrides the legacy package argument when non-nil
	TargetDir      *string `json:"targetDir"`      // Directory to evaluate (for non-recursive), nil = module root
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

	// Select the requested package during recursive discovery so unrelated
	// packages and their imports are never loaded as workspace instances.
	loaderPackage := effectivePackageName

	cfg := &load.Config{
		Dir:        evalDir,
		ModuleRoot: goModuleRoot,
		Registry:   registry,
		Package:    loaderPackage,
	}

	var loadPattern string
	if options.Recursive {
		loadPattern = "./..."
	} else {
		loadPattern = "."
	}

	// Load CUE instances using native CUE loader
	loadedInstances := load.Instances([]string{loadPattern}, cfg)
	if overlay, ok := attributedUnrelatedInvalidFileOverlay(loadedInstances, effectivePackageName); ok {
		cfg.Overlay = overlay
		loadedInstances = load.Instances([]string{loadPattern}, cfg)
	}
	if len(loadedInstances) == 0 {
		hint := "No CUE files found matching the load pattern"
		result = createErrorResponse(ErrorCodeLoadInstance, "No CUE instances found", &hint)
		return result
	}

	// The bridge does not require or load the cuenv schema separately. Instances
	// that import a schema are validated naturally during BuildInstance. Project
	// classification itself uses the concrete serialized "name" field rather
	// than performing an additional schema unification.

	// Keep only the selected package. Recursive evaluation is atomic for that
	// package: an error in any selected instance must fail the whole request
	// instead of returning a partial workspace.
	var validInstances []*build.Instance
	var loadErrors []string
	var packageMismatches []string
	for _, inst := range loadedInstances {
		if inst.Err != nil {
			if effectivePackageName == "" || inst.PkgName == effectivePackageName || inst.PkgName == "" || inst.PkgName == "_" {
				loadErrors = append(loadErrors, fmt.Sprintf("%s (package %q): %v", inst.Dir, inst.PkgName, inst.Err))
			} else {
				packageMismatches = append(packageMismatches, fmt.Sprintf("%s has package '%s'", inst.Dir, inst.PkgName))
			}
			continue
		}
		if effectivePackageName != "" && inst.PkgName != effectivePackageName {
			packageMismatches = append(packageMismatches, fmt.Sprintf("%s has package '%s'", inst.Dir, inst.PkgName))
			continue
		}
		validInstances = append(validInstances, inst)
	}
	if len(loadErrors) > 0 {
		hint := fmt.Sprintf("evalDir=%s, moduleRoot=%s, loadPattern=%s, package=%s, errors=%v",
			evalDir, goModuleRoot, loadPattern, effectivePackageName, loadErrors)
		result = createErrorResponse(ErrorCodeLoadInstance, "Failed to load selected CUE package or unattributed CUE input", &hint)
		return result
	}

	// Prepare result containers
	instances := make(map[string]json.RawMessage)
	projects := []string{} // Use empty slice, not nil, so JSON serializes as [] instead of null
	allMeta := make(map[string]ValueMeta)
	var buildErrors []string

	// Build CUE values SEQUENTIALLY to avoid race conditions.
	// CUE's build.Instance objects share internal state (file caches, parsed ASTs),
	// so concurrent BuildInstance calls on different instances can race.
	type builtInstance struct {
		relPath string
		value   cue.Value
		inst    *build.Instance // Needed for meta extraction
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
			buildErrors = append(buildErrors, fmt.Sprintf("%s: %v", relPath, v.Err()))
			continue
		}

		// Inject sequence item _name fields so that computed output ref fields
		// (stdout, stderr, exitCode) resolve to concrete values everywhere.
		v = injectTaskNames(v)

		builtInstances = append(builtInstances, builtInstance{
			relPath: relPath,
			value:   v,
			inst:    inst,
		})
	}
	if len(buildErrors) > 0 {
		hint := fmt.Sprintf("evalDir=%s, moduleRoot=%s, loadPattern=%s, package=%s, errors=%v",
			evalDir, goModuleRoot, loadPattern, effectivePackageName, buildErrors)
		result = createErrorResponse(ErrorCodeBuildValue, "Failed to build selected CUE package", &hint)
		return result
	}

	moduleRoot := goModuleRoot
	withMeta := options.WithMeta
	withReferences := options.WithReferences

	// Serialize every selected instance before producing any response data.
	// Values from one cue.Context share evaluator caches, so this remains
	// sequential.
	type serializedInstance struct {
		builtInstance
		jsonBytes []byte
	}
	serializedInstances := make([]serializedInstance, 0, len(builtInstances))
	var serializationErrors []string
	for _, built := range builtInstances {
		jsonBytes, err := buildJSONClean(built.value)
		if err != nil {
			serializationErrors = append(serializationErrors, fmt.Sprintf("%s: %v", built.relPath, err))
			continue
		}
		serializedInstances = append(serializedInstances, serializedInstance{
			builtInstance: built,
			jsonBytes:     jsonBytes,
		})
	}
	if len(serializationErrors) > 0 {
		hint := fmt.Sprintf("evalDir=%s, moduleRoot=%s, loadPattern=%s, package=%s, errors=%v",
			evalDir, goModuleRoot, loadPattern, effectivePackageName, serializationErrors)
		result = createErrorResponse(ErrorCodeOrderedJSON, "Failed to serialize selected CUE package", &hint)
		return result
	}

	// Extract response data only after every selected instance has built and
	// serialized successfully.
	for _, serialized := range serializedInstances {
		built := serialized.builtInstance
		jsonBytes := serialized.jsonBytes
		instances[built.relPath] = json.RawMessage(jsonBytes)
		// Classify from the serialized value rather than issuing another CUE
		// lookup against the shared evaluator context. Workspace sync consumes
		// the serialized value, so project membership must be derived from that
		// same stable snapshot.
		if hasConcreteProjectName(jsonBytes) {
			projects = append(projects, built.relPath)
		}

		if withMeta {
			meta := extractFieldMetaSeparate(built.inst, moduleRoot, built.relPath)
			definitionMeta := extractValueMetaSeparate(built.value, moduleRoot, built.relPath)
			for k, definition := range definitionMeta {
				existing := meta[k]
				existing.DefinitionDirectory = definition.DefinitionDirectory
				existing.DefinitionFilename = definition.DefinitionFilename
				existing.DefinitionLine = definition.DefinitionLine
				meta[k] = existing
			}

			for k, v := range meta {
				allMeta[k] = v
			}
		}

		if withReferences {
			refs := make(map[string]string)
			// Extract from evaluated value for canonical paths (resolves let bindings).
			extractReferencesFromValue(built.value, built.relPath, "", refs)
			// Fall back to AST extraction for other references (backwards compat).
			astRefs := extractReferencesFromAST(built.inst, built.relPath)
			for k, v := range astRefs {
				if _, exists := refs[k]; !exists {
					refs[k] = v
				}
			}

			// Merge reference paths into meta entries.
			for k, refPath := range refs {
				if existing, ok := allMeta[k]; ok {
					existing.Reference = refPath
					allMeta[k] = existing
				} else {
					// Create a meta entry with just the reference if no source position exists.
					allMeta[k] = ValueMeta{Reference: refPath}
				}
			}
		}
	}

	if len(instances) == 0 {
		hint := fmt.Sprintf("evalDir=%s, moduleRoot=%s, loadPattern=%s, package=%s, loadedInstances=%d, selectedInstances=%d, packageMismatches=%v",
			evalDir, goModuleRoot, loadPattern, effectivePackageName, len(loadedInstances), len(validInstances), packageMismatches)
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

// attributedUnrelatedInvalidFileOverlay returns minimal replacements for
// syntax-invalid files whose package clauses are all recoverable and do not
// match selectedPackage. The caller reruns the same recursive selected-package
// load with this in-memory overlay and only evaluates that clean result. Any
// missing, selected, dependency, or mixed attribution remains fail-closed.
func attributedUnrelatedInvalidFileOverlay(instances []*build.Instance, selectedPackage string) (map[string]load.Source, bool) {
	if selectedPackage == "" {
		return nil, false
	}

	overlay := make(map[string]load.Source)
	foundError := false
	for _, inst := range instances {
		if inst.Err == nil {
			continue
		}
		foundError = true
		if len(inst.InvalidFiles) == 0 || len(inst.DepsErrors) > 0 || inst.ResolutionErr != nil {
			return nil, false
		}

		invalidPaths := make(map[string]struct{}, len(inst.InvalidFiles))
		for _, file := range inst.InvalidFiles {
			parsed, err := cueparser.ParseFile(file.Filename, file.Source, cueparser.PackageClauseOnly)
			if err != nil || parsed == nil {
				return nil, false
			}

			packageName := parsed.PackageName()
			if packageName == "" || packageName == selectedPackage {
				return nil, false
			}
			filename := filepath.Clean(file.Filename)
			invalidPaths[filename] = struct{}{}
			overlay[filename] = load.FromString(fmt.Sprintf("package %s\n", packageName))
		}

		for _, loadErr := range cueerrors.Errors(inst.Err) {
			positions := cueerrors.Positions(loadErr)
			if len(positions) == 0 {
				return nil, false
			}

			attributed := false
			for _, position := range positions {
				filename := position.Filename()
				if filename == "" {
					continue
				}
				if _, exists := invalidPaths[filepath.Clean(filename)]; !exists {
					return nil, false
				}
				attributed = true
			}
			if !attributed {
				return nil, false
			}
		}
	}

	return overlay, foundError && len(overlay) > 0
}

func hasConcreteProjectName(jsonBytes []byte) bool {
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(jsonBytes, &fields); err != nil {
		return false
	}

	rawName, exists := fields["name"]
	if !exists {
		return false
	}

	var name string
	return json.Unmarshal(rawName, &name) == nil
}

// injectTaskNames walks the "tasks" struct in a CUE value and fills the hidden
// _name field on task nodes that live inside sequences. Named tasks and group
// children derive _name directly in schema via label aliases; sequence items
// still need bridge-side injection because CUE does not yet support aliases on
// list elements.
func injectTaskNames(v cue.Value) cue.Value {
	tasksVal := v.LookupPath(cue.ParsePath("tasks"))
	if !tasksVal.Exists() || tasksVal.Err() != nil {
		return v
	}

	return injectTaskNamesRecursive(v, tasksVal, "")
}

// injectTaskNamesRecursive walks task nodes and fills _name for sequence items.
func injectTaskNamesRecursive(root cue.Value, node cue.Value, prefix string) cue.Value {
	switch node.Kind() {
	case cue.StructKind:
		// Check if this struct looks like a Task (has "command" or "script" field)
		if isTaskShaped(node) {
			if strings.Contains(prefix, "[") {
				root = fillTaskName(root, prefix)
			}
			return root
		}

		// Check if this is a TaskGroup (has type: "group")
		typeField := node.LookupPath(cue.ParsePath("type"))
		if typeField.Exists() && typeField.Err() == nil {
			if s, err := typeField.String(); err == nil && s == "group" {
				// Walk group children (skip known group fields)
				iter, _ := node.Fields(cue.Definitions(false))
				for iter.Next() {
					label := iter.Label()
					if label == "type" || label == "dependsOn" || label == "maxConcurrency" || label == "description" {
						continue
					}
					childPrefix := label
					if prefix != "" {
						childPrefix = prefix + "." + label
					}
					root = injectTaskNamesRecursive(root, iter.Value(), childPrefix)
				}
				return root
			}
		}

		// Otherwise treat as a struct with named task children
		iter, _ := node.Fields(cue.Definitions(false))
		for iter.Next() {
			label := iter.Label()
			childPrefix := label
			if prefix != "" {
				childPrefix = prefix + "." + label
			}
			root = injectTaskNamesRecursive(root, iter.Value(), childPrefix)
		}

	case cue.ListKind:
		// Sequence: walk each element
		list, _ := node.List()
		for i := 0; list.Next(); i++ {
			childPrefix := fmt.Sprintf("%s[%d]", prefix, i)
			root = injectTaskNamesRecursive(root, list.Value(), childPrefix)
		}
	}

	return root
}

// isTaskShaped returns true if the CUE value looks like a #Task
// (has a "command" or "script" field).
func isTaskShaped(v cue.Value) bool {
	cmd := v.LookupPath(cue.ParsePath("command"))
	if cmd.Exists() && cmd.Err() == nil {
		return true
	}
	scr := v.LookupPath(cue.ParsePath("script"))
	return scr.Exists() && scr.Err() == nil
}

// fillTaskName fills the _name hidden field on a sequence task at the given path.
func fillTaskName(root cue.Value, taskName string) cue.Value {
	if taskName == "" {
		return root
	}

	namePath, ok := taskFillPath(taskName)
	if !ok {
		return root
	}

	return root.FillPath(namePath, taskName)
}

// taskFillPath converts a task path like "pipeline[0]" or
// "release-check[0].verify" into a CUE FillPath that targets tasks.<path>._name.
func taskFillPath(taskName string) (cue.Path, bool) {
	selectors := []cue.Selector{cue.Str("tasks")}

	for i := 0; i < len(taskName); {
		labelStart := i
		for i < len(taskName) && taskName[i] != '.' && taskName[i] != '[' {
			i++
		}
		if labelStart != i {
			selectors = append(selectors, cue.Str(taskName[labelStart:i]))
		}

		for i < len(taskName) && taskName[i] == '[' {
			i++
			indexStart := i
			for i < len(taskName) && taskName[i] != ']' {
				i++
			}
			if i == len(taskName) || indexStart == i {
				return cue.Path{}, false
			}

			index, err := strconv.Atoi(taskName[indexStart:i])
			if err != nil || index < 0 {
				return cue.Path{}, false
			}
			selectors = append(selectors, cue.Index(index))
			i++
		}

		if i == len(taskName) {
			break
		}
		if taskName[i] != '.' {
			return cue.Path{}, false
		}
		i++
		if i == len(taskName) {
			return cue.Path{}, false
		}
	}

	selectors = append(selectors, cue.Hid("_name", schemaPackagePath))
	return cue.MakePath(selectors...), true
}

// schemaPackagePath is the CUE import path for the schema package.
// Hidden fields (_name) are scoped to their defining package, so FillPath
// needs the full package path to target them.
const schemaPackagePath = "github.com/cuenv/cuenv/schema"

func main() {}
