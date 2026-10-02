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
	"sort"
	"strconv"
	"strings"
	"unsafe"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/load"
	"cuelang.org/go/cue/parser"
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
// packageFilterOverlay blanks files from an exact target directory whose
// syntax-aware package clause does not match the requested package. CUE's
// package-specific loader otherwise reports malformed unrelated files as
// errors before it can select the requested package.
func packageFilterOverlay(evalDir, packageName string, recursive bool) (map[string]load.Source, bool, error) {
	if packageName == "" || recursive {
		return nil, false, nil
	}
	overlayDir, err := filepath.Abs(evalDir)
	if err != nil {
		return nil, false, err
	}
	entries, err := os.ReadDir(overlayDir)
	if err != nil {
		return nil, false, err
	}
	overlay := make(map[string]load.Source)
	matched := false
	for _, entry := range entries {
		if entry.IsDir() || filepath.Ext(entry.Name()) != ".cue" {
			continue
		}
		filename := filepath.Join(overlayDir, entry.Name())
		source, err := os.ReadFile(filename)
		if err != nil {
			return nil, false, err
		}
		syntax, parseErr := parser.ParseFile(filename, source)
		if syntax != nil && syntax.PackageName() == packageName {
			matched = true
			continue
		}
		if parseErr != nil && syntax == nil {
			// Without a partial AST the package cannot be identified safely;
			// leave the original source in place so the evaluator reports an
			// explicit failure rather than silently treating it as absent.
			matched = true
			continue
		}
		overlay[filename] = load.FromBytes([]byte("package _\n"))
	}
	return overlay, matched, nil
}

// invalidInstanceDeclaresPackage recovers the package clause from files that
// failed CUE parsing. The loader deliberately sets PkgName to "_" for those
// instances, so filtering only on build.Instance.PkgName would turn a broken
// matching package into neutral absence. ParseFile returns a partial AST on
// syntax errors, making this a syntax-aware check rather than a second source
// text parser.
func invalidInstanceDeclaresPackage(inst *build.Instance, packageName string) bool {
	if packageName == "" {
		return true
	}
	for _, file := range inst.InvalidFiles {
		if file.Encoding != build.CUE {
			continue
		}
		syntax, _ := parser.ParseFile(file.Filename, file.Source)
		if syntax != nil && syntax.PackageName() == packageName {
			return true
		}
	}
	return false
}

func createErrorResponse(code, message string, hint *string) string {
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
		return fmt.Sprintf(`{"version":"%s","error":{"code":"%s","message":"Failed to marshal error response: %s"}}`, BridgeVersion, ErrorCodeJSONMarshal, err.Error())
	}
	return string(responseBytes)
}

// Helper function to create success response
func createSuccessResponse(data string) string {
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
	return string(responseBytes)
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
	return C.CString(moduleDependencyVersionResponse(C.GoString(moduleRootPath), C.GoString(dependencyPath)))
}

// moduleDependencyVersionResponse implements cue_module_dependency_version
// on Go strings so it can be exercised without cgo.
func moduleDependencyVersionResponse(moduleRoot string, dependencyBasePath string) (result string) {
	defer func() {
		if r := recover(); r != nil {
			panicMsg := fmt.Sprintf("Internal panic: %v", r)
			result = createErrorResponse(ErrorCodePanicRecover, panicMsg, nil)
		}
	}()

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

// ModuleResult contains all evaluated instances in a module
type ModuleResult struct {
	Instances map[string]json.RawMessage `json:"instances"`
	Projects  []string                   `json:"projects"`       // paths that conform to schema.#Project
	Meta      map[string]ValueMeta       `json:"meta,omitempty"` // "path/field" -> source location
	// Present maps each exported instance key to the presencePaths that
	// exist in it (only when presencePaths is set).
	Present map[string][]string `json:"present,omitempty"`
	// SkippedDirectories lists the directories a recursive evaluation left
	// out although they hold CUE files (only with skippedDirectories
	// "report").
	SkippedDirectories []SkippedDirectory `json:"skippedDirectories,omitempty"`
}

// ModuleEvalOptions controls how module evaluation behaves
type ModuleEvalOptions struct {
	WithMeta       bool    `json:"withMeta"`       // Extract source positions into separate Meta map
	WithReferences bool    `json:"withReferences"` // Extract reference paths (requires WithMeta)
	Recursive      bool    `json:"recursive"`      // true: cue eval ./..., false: cue eval .
	PackageName    *string `json:"packageName"`    // Filter to specific package, nil = all packages
	TargetDir      *string `json:"targetDir"`      // Directory to evaluate (for non-recursive), nil = module root
	// ConcretePaths lists CUE paths (cue.ParsePath syntax, nested selectors
	// allowed) that must exist and be fully concrete in every evaluated
	// instance. A malformed path is an input error; a missing or
	// non-concrete path fails that instance.
	ConcretePaths []string `json:"concretePaths"`
	// InstanceFailures decides what happens to a loaded instance that fails
	// to load, build, validate or export. InstanceFailuresSkip (the default,
	// also selected by an empty value) leaves it out of the result as long as
	// another instance succeeds. InstanceFailuresFail fails the whole
	// evaluation with an error naming every failed instance. Instances
	// excluded by the package filter are not failures.
	InstanceFailures string `json:"instanceFailures"`
	// PackageScope selects which packages are evaluated. PackageScopeNamed
	// (the default, also selected by an empty value) evaluates the package
	// named by PackageName or the legacy parameter, or the single package of
	// each directory when no name is given; instances are keyed by their
	// directory relative to the module root. PackageScopeAll evaluates every
	// package in every loaded directory, including directories holding
	// several packages, and keys each instance as "<directory>:<package>"
	// (for example ".:app" or "services/api:worker"); no package name may be
	// given with it. Meta and projects entries use the same keys.
	PackageScope string `json:"packageScope"`
	// ExportPaths, when not empty, limits each exported instance to these
	// regular-field paths (CUE path syntax), nested as in the instance; a
	// path an instance lacks is left out. Loading, building, concretePaths
	// and instanceFailures are unchanged: only the export is smaller.
	ExportPaths []string `json:"exportPaths"`
	// PresencePaths lists regular-field paths whose existence is reported
	// per instance in ModuleResult.Present, without exporting their values.
	PresencePaths []string `json:"presencePaths"`
	// TaskField names the top-level field that holds the task graph. Sequence
	// items inside it get their hidden `_name` field injected before export
	// (see injectTaskNames), and a projection that exports nothing below this
	// field skips the injection. nil selects defaultTaskField, the behaviour
	// of callers that predate this option; an empty string turns the
	// injection off.
	TaskField *string `json:"taskField"`
	// SkippedDirectories decides whether a recursive evaluation reports the
	// directories it leaves out (SkippedDirectoriesReport) or not
	// (SkippedDirectoriesIgnore, the default, also selected by an empty
	// value). The rules follow CUE's "./..." walk: below the walk root,
	// directories whose name starts with "." or "_", directories named
	// "testdata", and directories holding their own cue.mod are not loaded.
	SkippedDirectories string `json:"skippedDirectories"`
}

// Values accepted by ModuleEvalOptions.PackageScope.
const (
	PackageScopeNamed = "named"
	PackageScopeAll   = "all"
)

// Values accepted by ModuleEvalOptions.InstanceFailures.
const (
	InstanceFailuresSkip = "skip"
	InstanceFailuresFail = "fail"
)

//export cue_eval_module
func cue_eval_module(moduleRootPath *C.char, packageName *C.char, optionsJSON *C.char) *C.char {
	return C.CString(evaluateModuleResponse(C.GoString(moduleRootPath), C.GoString(packageName), C.GoString(optionsJSON)))
}

// evaluateModuleResponse implements cue_eval_module on Go strings and returns
// the JSON bridge envelope. Keeping the C boundary in the exported wrapper
// lets Go tests exercise evaluation without cgo in test files.
//
// goPackageName is the legacy package parameter kept for backwards
// compatibility; options.packageName takes precedence over it.
func evaluateModuleResponse(goModuleRoot string, goPackageName string, goOptionsJSON string) (result string) {
	// Recover from panics so the caller always receives a bridge envelope.
	defer func() {
		if r := recover(); r != nil {
			panicMessage := fmt.Sprintf("Internal panic: %v", r)
			result = createErrorResponse(ErrorCodePanicRecover, panicMessage, nil)
		}
	}()

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

	switch options.InstanceFailures {
	case "", InstanceFailuresSkip, InstanceFailuresFail:
	default:
		hint := fmt.Sprintf("instanceFailures must be %q or %q", InstanceFailuresSkip, InstanceFailuresFail)
		result = createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Unknown instanceFailures value %q", options.InstanceFailures), &hint)
		return result
	}

	concretePaths, err := parseConcretePaths(options.ConcretePaths)
	if err != nil {
		hint := "concretePaths entries use CUE path syntax, for example \"config\" or \"config.database\""
		result = createErrorResponse(ErrorCodeInvalidInput, err.Error(), &hint)
		return result
	}
	projectionHint := "exportPaths and presencePaths entries use CUE path syntax with regular fields, for example \"name\" or \"config.database\""
	exportPaths, err := parseProjectionPaths("exportPaths", options.ExportPaths)
	if err != nil {
		result = createErrorResponse(ErrorCodeInvalidInput, err.Error(), &projectionHint)
		return result
	}
	presencePaths, err := parseProjectionPaths("presencePaths", options.PresencePaths)
	if err != nil {
		result = createErrorResponse(ErrorCodeInvalidInput, err.Error(), &projectionHint)
		return result
	}

	taskField := defaultTaskField
	if options.TaskField != nil {
		taskField = *options.TaskField
	}

	switch options.SkippedDirectories {
	case "", SkippedDirectoriesIgnore, SkippedDirectoriesReport:
	default:
		hint := fmt.Sprintf("skippedDirectories must be %q or %q", SkippedDirectoriesIgnore, SkippedDirectoriesReport)
		result = createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Unknown skippedDirectories value %q", options.SkippedDirectories), &hint)
		return result
	}

	// PackageName from options takes precedence over legacy parameter
	effectivePackageName := goPackageName
	if options.PackageName != nil {
		effectivePackageName = *options.PackageName
	}

	allPackages := false
	switch options.PackageScope {
	case "", PackageScopeNamed:
	case PackageScopeAll:
		if effectivePackageName != "" {
			hint := fmt.Sprintf("Pass an empty package name with packageScope %q", PackageScopeAll)
			result = createErrorResponse(ErrorCodeInvalidInput,
				fmt.Sprintf("packageScope %q evaluates every package and cannot be combined with package %q", PackageScopeAll, effectivePackageName), &hint)
			return result
		}
		allPackages = true
	default:
		hint := fmt.Sprintf("packageScope must be %q or %q", PackageScopeNamed, PackageScopeAll)
		result = createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Unknown packageScope value %q", options.PackageScope), &hint)
		return result
	}

	// Validate inputs
	if goModuleRoot == "" {
		result = createErrorResponse(ErrorCodeInvalidInput, "Module root path cannot be empty", nil)
		return result
	}
	absoluteModuleRoot, err := filepath.Abs(goModuleRoot)
	if err != nil {
		hint := "Ensure the CUE root path is valid"
		result = createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Cannot resolve CUE root path: %v", err), &hint)
		return result
	}
	goModuleRoot = absoluteModuleRoot

	// A module root is preferred for imports and dependency resolution, but a
	// package-filtered evaluation may intentionally target a standalone CUE
	// directory. The loader can still evaluate that exact directory with the
	// supplied root; imported module paths will report a normal CUE error.
	moduleFile := filepath.Join(goModuleRoot, "cue.mod", "module.cue")
	if _, err := os.Stat(moduleFile); err != nil && !os.IsNotExist(err) {
		hint := "Ensure the CUE root is readable"
		result = createErrorResponse(ErrorCodeInvalidInput, "Cannot inspect CUE root", &hint)
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
		if !filepath.IsAbs(evalDir) {
			evalDir, err = filepath.Abs(evalDir)
			if err != nil {
				hint := "Ensure the exact CUE target directory is valid"
				result = createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Cannot resolve CUE target path: %v", err), &hint)
				return result
			}
		}
	}

	// Recursive package queries need all package instances so the result can be
	// filtered below. Exact package queries use the loader's package selector,
	// with an overlay that blanks unrelated files in the target directory. This
	// keeps a malformed unrelated file from poisoning a valid requested package
	// while preserving syntax/build errors in matching files.
	loaderPackage := effectivePackageName
	if (effectivePackageName != "" && options.Recursive) || allPackages {
		loaderPackage = "*"
	}
	packageOverlay, packageMatched, err := packageFilterOverlay(evalDir, effectivePackageName, options.Recursive)
	if err != nil {
		hint := "Ensure the exact CUE target directory is readable"
		result = createErrorResponse(ErrorCodeLoadInstance, fmt.Sprintf("Cannot inspect CUE target: %v", err), &hint)
		return result
	}
	if effectivePackageName != "" && !options.Recursive && !packageMatched {
		result = createSuccessResponse(`{"instances":{},"projects":[]}`)
		return result
	}

	cfg := &load.Config{
		Dir:        evalDir,
		ModuleRoot: goModuleRoot,
		Registry:   registry,
		Package:    loaderPackage,
		Overlay:    packageOverlay,
	}

	// A recursive load walks the directories itself (with CUE's "./..." rules,
	// see recursiveDirectories) and hands the loader the explicit list, so a
	// root directory named ".x" or "_x" loads like any other root. loadPattern
	// names the request in errors.
	loadPattern := "."
	loadPatterns := []string{loadPattern}
	var skippedDirectories []SkippedDirectory
	if options.Recursive {
		loadPattern = "./..."
		loadPatterns, skippedDirectories = recursiveDirectories(evalDir, goModuleRoot, options.SkippedDirectories)
	}

	// NOTE: We intentionally do NOT append ":packageName" to the load pattern.
	// Recursive queries load Package:"*" and filter by package name below; exact
	// queries use Config.Package plus the package-filter overlay above. Using
	// "./...:cuenv" causes CUE to create instances for EVERY directory by
	// unifying ancestor package files, not just directories with .cue files.

	// Load CUE instances using native CUE loader
	var loadedInstances []*build.Instance
	if len(loadPatterns) > 0 {
		loadedInstances = load.Instances(loadPatterns, cfg)
	}
	if options.Recursive || allPackages {
		// Instances without files of their own (see withoutEmptyInstances)
		// are not packages anyone wrote.
		loadedInstances = withoutEmptyInstances(loadedInstances)
	}
	if len(loadedInstances) == 0 {
		// A package-filtered query is also a presence query for callers such as
		// Cuetty. No files in the exact target directory means the requested
		// package is absent, not that CUE evaluation itself failed. Returning an
		// empty result keeps absence distinct from syntax/build errors below.
		if effectivePackageName != "" {
			result = emptyResultResponse(skippedDirectories)
			return result
		}
		message := fmt.Sprintf("No CUE instances found in %s", evalDir)
		for _, skipped := range skippedDirectories {
			message += fmt.Sprintf("\nnot loaded: %s (%s)", skipped.Path, skipped.Reason)
		}
		result = createErrorResponse(ErrorCodeLoadInstance, message, nil)
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
			// CUE represents an exact directory with no package as an error
			// instance whose message says it "matched no packages". For a
			// package-filtered presence query that is neutral absence, not a
			// syntax/build failure. Other loader errors must remain explicit.
			if effectivePackageName != "" && strings.Contains(inst.Err.Error(), "matched no packages") {
				packageMismatches = append(packageMismatches, fmt.Sprintf("%s has no package", inst.Dir))
				continue
			}
			if effectivePackageName != "" && inst.PkgName != effectivePackageName && !invalidInstanceDeclaresPackage(inst, effectivePackageName) {
				packageMismatches = append(packageMismatches, fmt.Sprintf("%s has no package '%s'", inst.Dir, effectivePackageName))
				continue
			}
			// A loader-level failure (for example two packages in one
			// directory of an unfiltered recursive load) has no instance
			// directory; name the load pattern instead.
			failedInstance := loadPattern
			if inst.Dir != "" {
				failedInstance = instanceKey(goModuleRoot, inst, allPackages)
			}
			loadErrors = append(loadErrors, instanceFailure(failedInstance, inst.Err, goModuleRoot))
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
	present := make(map[string][]string)
	var buildErrors []string

	// Build and export CUE values SEQUENTIALLY to avoid race conditions.
	// CUE's build.Instance objects share internal state (file caches, parsed
	// ASTs), so concurrent BuildInstance calls on different instances can
	// race; read-looking APIs such as Fields, Decode, and ReferencePath can
	// mutate evaluator state. Each instance is built in its own context and
	// exported right after it is built, then dropped, so the evaluated values
	// of a large module are not all held in memory at once.
	type builtInstance struct {
		relPath   string
		value     cue.Value
		isProject bool
		inst      *build.Instance // Needed for meta extraction
	}

	moduleRoot := goModuleRoot
	withMeta := options.WithMeta
	withReferences := options.WithReferences

	exportInstance := func(built builtInstance) {
		var jsonBytes []byte
		var exportErr error
		if len(exportPaths) > 0 {
			jsonBytes, exportErr = buildProjectedJSON(built.value, exportPaths)
		} else {
			jsonBytes, exportErr = buildJSONClean(built.value)
		}
		if exportErr != nil {
			buildErrors = append(buildErrors, instanceFailure(built.relPath, exportErr, goModuleRoot))
			return // Skip failed instances
		}
		instances[built.relPath] = json.RawMessage(jsonBytes)
		if len(presencePaths) > 0 {
			present[built.relPath] = presentPaths(built.value, presencePaths)
		}
		if built.isProject {
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

	for _, inst := range validInstances {
		relPath := instanceKey(goModuleRoot, inst, allPackages)

		// Build the CUE value (must be sequential). Each instance gets its
		// own context: a context keeps every instance built with it alive,
		// so one shared context grows with the whole module (about 200 MB
		// per instance of 200 schema-checked tasks), while imported
		// packages are cheap to rebuild. Values of different instances are
		// never combined.
		ctx := cuecontext.New()
		v := ctx.BuildInstance(inst)
		if v.Err() != nil {
			// Collect build errors so they can be reported if no instances succeed
			buildErrors = append(buildErrors, instanceFailure(relPath, allErrors(v), goModuleRoot))
			continue
		}

		// Caller-named fields must be concrete; see validateConcretePaths.
		if err := validateConcretePaths(v, concretePaths, goModuleRoot); err != nil {
			buildErrors = append(buildErrors, instanceFailure(relPath, err, goModuleRoot))
			continue
		}

		// Inject sequence item _name fields so that computed output ref fields
		// (stdout, stderr, exitCode) resolve to concrete values everywhere.
		// A projection that exports no tasks does not need them.
		if exportsField(exportPaths, taskField) {
			v = injectTaskNames(v, taskField)
		}

		// Check if this is a Project (has required "name" field) vs Base (no name)
		isProject := false
		nameField := v.LookupPath(cue.ParsePath("name"))
		if nameField.Exists() && nameField.Err() == nil {
			isProject = true
		}

		exportInstance(builtInstance{
			relPath:   relPath,
			value:     v,
			isProject: isProject,
			inst:      inst,
		})
	}

	if options.InstanceFailures == InstanceFailuresFail && (len(loadErrors) > 0 || len(buildErrors) > 0) {
		failures := append(append([]string{}, loadErrors...), buildErrors...)
		sort.Strings(failures)
		message := fmt.Sprintf("%d instance(s) could not be evaluated:\n%s", len(failures), strings.Join(failures, "\n"))
		result = createErrorResponse(ErrorCodeBuildValue, message, nil)
		return result
	}

	if len(instances) == 0 {
		// A package filter can legitimately match no instance when the target
		// directory contains only another package (or no package declaration).
		// Preserve that as an empty success; load/build errors remain failures.
		if effectivePackageName != "" && len(loadErrors) == 0 && len(buildErrors) == 0 {
			result = emptyResultResponse(skippedDirectories)
			return result
		}
		failures := append(append([]string{}, loadErrors...), buildErrors...)
		sort.Strings(failures)
		message := "No instances could be evaluated"
		if len(failures) > 0 {
			message = fmt.Sprintf("No instances could be evaluated:\n%s", strings.Join(failures, "\n"))
		} else if len(packageMismatches) > 0 {
			message = fmt.Sprintf("No instances could be evaluated: %s", strings.Join(packageMismatches, "; "))
		}
		result = createErrorResponse(ErrorCodeBuildValue, message, nil)
		return result
	}

	// Marshal the result
	moduleResult := ModuleResult{
		Instances:          instances,
		Projects:           projects,
		SkippedDirectories: skippedDirectories,
	}
	if len(presencePaths) > 0 {
		moduleResult.Present = present
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

// relativeInstancePath names an instance by its directory relative to the
// module root ("." for the root itself), falling back to the absolute
// directory when it is not below the root.
func relativeInstancePath(moduleRoot string, directory string) string {
	relPath, err := filepath.Rel(moduleRoot, directory)
	if err != nil {
		return directory
	}
	if relPath == "" {
		return "."
	}
	return relPath
}

// instanceKey is the result key of an instance: its directory relative to
// the module root, qualified as "<directory>:<package>" when every package is
// evaluated (a directory can then hold several instances). CUE package names
// are identifiers, so the key splits unambiguously at its last colon.
func instanceKey(moduleRoot string, inst *build.Instance, allPackages bool) string {
	directory := relativeInstancePath(moduleRoot, inst.Dir)
	if !allPackages || inst.PkgName == "" {
		// A load failure may not know its package; name the directory.
		return directory
	}
	return directory + ":" + inst.PkgName
}

// injectTaskNames walks the struct at the given top-level field in a CUE value and fills the hidden
// _name field on task nodes that live inside sequences. Named tasks and group
// children derive _name directly in schema via label aliases; sequence items
// still need bridge-side injection because CUE does not yet support aliases on
// list elements.
func injectTaskNames(v cue.Value, field string) cue.Value {
	if field == "" {
		return v
	}
	tasksVal := v.LookupPath(cue.MakePath(cue.Str(field)))
	if !tasksVal.Exists() || tasksVal.Err() != nil {
		return v
	}

	return injectTaskNamesRecursive(v, field, tasksVal, "")
}

// injectTaskNamesRecursive walks task nodes and fills _name for sequence items.
func injectTaskNamesRecursive(root cue.Value, field string, node cue.Value, prefix string) cue.Value {
	switch node.Kind() {
	case cue.StructKind:
		// Check if this struct looks like a Task (has "command" or "script" field)
		if isTaskShaped(node) {
			if strings.Contains(prefix, "[") {
				root = fillTaskName(root, field, prefix)
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
					root = injectTaskNamesRecursive(root, field, iter.Value(), childPrefix)
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
			root = injectTaskNamesRecursive(root, field, iter.Value(), childPrefix)
		}

	case cue.ListKind:
		// Sequence: walk each element
		list, _ := node.List()
		for i := 0; list.Next(); i++ {
			childPrefix := fmt.Sprintf("%s[%d]", prefix, i)
			root = injectTaskNamesRecursive(root, field, list.Value(), childPrefix)
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
func fillTaskName(root cue.Value, field string, taskName string) cue.Value {
	if taskName == "" {
		return root
	}

	namePath, ok := taskFillPath(field, taskName)
	if !ok {
		return root
	}

	return root.FillPath(namePath, taskName)
}

// taskFillPath converts a task path like "pipeline[0]" or
// "release-check[0].verify" into a CUE FillPath that targets
// <field>.<path>._name.
func taskFillPath(field string, taskName string) (cue.Path, bool) {
	selectors := []cue.Selector{cue.Str(field)}

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

// defaultTaskField is the task graph field used when ModuleEvalOptions.TaskField
// is not set. It keeps the behaviour of callers that do not pass the option.
const defaultTaskField = "tasks"

// schemaPackagePath is the CUE import path for the schema package.
// Hidden fields (_name) are scoped to their defining package, so FillPath
// needs the full package path to target them.
const schemaPackagePath = "github.com/cuenv/cuenv/schema"

func main() {}
