package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// Tests call the Go-string implementations behind the exported cgo symbols
// (evaluateModuleResponse, moduleDependencyVersionResponse) because Go does
// not allow cgo in test files. cue_free_string is exercised from the Rust
// side, which owns every string the bridge returns.

const (
	testModulePath  = "example.com/bridge"
	testPackageName = "app"
)

// writeCueModule creates a CUE module in a temporary directory. files maps a
// slash-separated path relative to the module root to its contents.
func writeCueModule(t *testing.T, files map[string]string) string {
	t.Helper()
	moduleRoot := t.TempDir()
	allFiles := map[string]string{
		"cue.mod/module.cue": fmt.Sprintf("module: %q\nlanguage: version: \"v0.14.1\"\n", testModulePath+"@v0"),
	}
	for name, contents := range files {
		allFiles[name] = contents
	}
	for name, contents := range allFiles {
		filename := filepath.Join(moduleRoot, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(filename), 0o755); err != nil {
			t.Fatalf("create directory for %s: %v", name, err)
		}
		if err := os.WriteFile(filename, []byte(contents), 0o644); err != nil {
			t.Fatalf("write %s: %v", name, err)
		}
	}
	return moduleRoot
}

// writePackage creates a module whose root holds a single file of the test
// package with the given body.
func writePackage(t *testing.T, contents string) string {
	t.Helper()
	return writeCueModule(t, map[string]string{"values.cue": packageSource(contents)})
}

func packageSource(contents string) string {
	return "package " + testPackageName + "\n\n" + contents
}

// testEnvelope mirrors BridgeResponse with the success payload decoded.
type testEnvelope struct {
	Version string          `json:"version"`
	Ok      json.RawMessage `json:"ok"`
	Error   *BridgeError    `json:"error"`
}

// evaluation describes one call to evaluateModuleResponse.
type evaluation struct {
	moduleRoot string
	// targetDirectory is relative to moduleRoot; empty means the root.
	targetDirectory  string
	packageName      *string
	recursive        bool
	withMeta         bool
	concretePaths    []string
	instanceFailures string
}

func packageNamed(name string) *string {
	return &name
}

func (e evaluation) optionsJSON(t *testing.T) string {
	t.Helper()
	options := map[string]interface{}{
		"recursive":        e.recursive,
		"withMeta":         e.withMeta,
		"concretePaths":    e.concretePaths,
		"instanceFailures": e.instanceFailures,
	}
	if e.packageName != nil {
		options["packageName"] = *e.packageName
	}
	if !e.recursive {
		options["targetDir"] = filepath.Join(e.moduleRoot, filepath.FromSlash(e.targetDirectory))
	}
	encoded, err := json.Marshal(options)
	if err != nil {
		t.Fatalf("marshal options: %v", err)
	}
	return string(encoded)
}

// run evaluates and decodes the bridge envelope.
func (e evaluation) run(t *testing.T) testEnvelope {
	t.Helper()
	return decodeEnvelope(t, evaluateModuleResponse(e.moduleRoot, "", e.optionsJSON(t)))
}

// result evaluates and decodes the module result, failing on a bridge error.
func (e evaluation) result(t *testing.T) ModuleResult {
	t.Helper()
	envelope := e.run(t)
	if envelope.Error != nil {
		t.Fatalf("unexpected bridge error: %+v (hint: %s)", envelope.Error, errorHint(envelope.Error))
	}
	var result ModuleResult
	if err := json.Unmarshal(envelope.Ok, &result); err != nil {
		t.Fatalf("parse module result: %v\nresult: %s", err, envelope.Ok)
	}
	return result
}

// failure evaluates and returns the bridge error, failing on success.
func (e evaluation) failure(t *testing.T) *BridgeError {
	t.Helper()
	envelope := e.run(t)
	if envelope.Error == nil {
		t.Fatalf("expected a bridge error, got: %s", envelope.Ok)
	}
	t.Logf("bridge error: %s\nhint: %s", envelope.Error.Message, errorHint(envelope.Error))
	return envelope.Error
}

// failureText is the error message and hint joined, for fragment checks.
func (e evaluation) failureText(t *testing.T) string {
	t.Helper()
	bridgeError := e.failure(t)
	return bridgeError.Message + "\n" + errorHint(bridgeError)
}

// instance evaluates and returns the decoded value of the instance at
// instancePath (as keyed in the module result).
func (e evaluation) instance(t *testing.T, instancePath string) map[string]interface{} {
	t.Helper()
	raw := e.rawInstance(t, instancePath)
	var value map[string]interface{}
	if err := json.Unmarshal(raw, &value); err != nil {
		t.Fatalf("parse instance: %v\ninstance: %s", err, raw)
	}
	return value
}

func (e evaluation) rawInstance(t *testing.T, instancePath string) json.RawMessage {
	t.Helper()
	result := e.result(t)
	raw, ok := result.Instances[instancePath]
	if !ok {
		t.Fatalf("instance %q missing from result: %v", instancePath, instanceNames(result))
	}
	return raw
}

// exactPackage is a non-recursive evaluation of the test package at the
// module root.
func exactPackage(moduleRoot string) evaluation {
	return evaluation{moduleRoot: moduleRoot, packageName: packageNamed(testPackageName)}
}

func decodeEnvelope(t *testing.T, response string) testEnvelope {
	t.Helper()
	var envelope testEnvelope
	if err := json.Unmarshal([]byte(response), &envelope); err != nil {
		t.Fatalf("parse bridge response: %v\nresponse: %s", err, response)
	}
	if envelope.Version != BridgeVersion {
		t.Fatalf("expected bridge version %q, got %q", BridgeVersion, envelope.Version)
	}
	return envelope
}

func instanceNames(result ModuleResult) []string {
	names := make([]string, 0, len(result.Instances))
	for name := range result.Instances {
		names = append(names, name)
	}
	return names
}

func errorHint(bridgeError *BridgeError) string {
	if bridgeError.Hint == nil {
		return ""
	}
	return *bridgeError.Hint
}

func assertContains(t *testing.T, text string, fragments ...string) {
	t.Helper()
	for _, fragment := range fragments {
		if !strings.Contains(text, fragment) {
			t.Errorf("expected %q in:\n%s", fragment, text)
		}
	}
}

func assertErrorCode(t *testing.T, bridgeError *BridgeError, code string) {
	t.Helper()
	if bridgeError.Code != code {
		t.Errorf("expected error code %s, got %s (%s)", code, bridgeError.Code, bridgeError.Message)
	}
}

// Input validation -----------------------------------------------------------

func TestEvaluateModule_EmptyModuleRoot(t *testing.T) {
	envelope := decodeEnvelope(t, evaluateModuleResponse("", testPackageName, ""))
	if envelope.Error == nil {
		t.Fatalf("expected an error, got: %s", envelope.Ok)
	}
	assertErrorCode(t, envelope.Error, ErrorCodeInvalidInput)
	assertContains(t, envelope.Error.Message, "Module root path cannot be empty")
}

func TestEvaluateModule_InvalidOptions(t *testing.T) {
	envelope := decodeEnvelope(t, evaluateModuleResponse(t.TempDir(), testPackageName, "{"))
	if envelope.Error == nil {
		t.Fatalf("expected an error, got: %s", envelope.Ok)
	}
	assertErrorCode(t, envelope.Error, ErrorCodeInvalidInput)
}

func TestEvaluateModule_NonexistentDirectory(t *testing.T) {
	missing := filepath.Join(t.TempDir(), "does", "not", "exist")

	// With a package filter the exact target directory cannot be inspected.
	filtered := exactPackage(missing).failure(t)
	assertErrorCode(t, filtered, ErrorCodeLoadInstance)

	// Without one the loader reports the missing directory; either way the
	// caller receives an error, never an empty success.
	unfiltered := evaluation{moduleRoot: missing}.failure(t)
	if unfiltered.Code != ErrorCodeLoadInstance && unfiltered.Code != ErrorCodeBuildValue {
		t.Errorf("expected a load or build error, got %s (%s)", unfiltered.Code, unfiltered.Message)
	}

	// A missing target directory inside an existing module fails the same way.
	moduleRoot := writePackage(t, `value: 1`)
	target := exactPackage(moduleRoot)
	target.targetDirectory = "missing"
	assertErrorCode(t, target.failure(t), ErrorCodeLoadInstance)
}

func TestEvaluateModule_EmptyPackageNameEvaluatesTheDirectoryPackage(t *testing.T) {
	// An empty package name is not a filter: the directory's only package is
	// evaluated, whatever it is called.
	moduleRoot := writeCueModule(t, map[string]string{
		"values.cue": "package other\n\nvalue: \"from other\"\n",
	})
	value := evaluation{moduleRoot: moduleRoot, packageName: packageNamed("")}.instance(t, ".")
	if value["value"] != "from other" {
		t.Errorf("unexpected value: %v", value)
	}

	// The legacy positional package parameter behaves the same when empty.
	envelope := decodeEnvelope(t, evaluateModuleResponse(moduleRoot, "", ""))
	if envelope.Error != nil {
		t.Fatalf("unexpected error: %+v", envelope.Error)
	}
	assertContains(t, string(envelope.Ok), `"value":"from other"`)
}

// Evaluation -----------------------------------------------------------------

func TestEvaluateModule_ValidInput(t *testing.T) {
	moduleRoot := writePackage(t, `
settings: {
	url:     "postgres://localhost/mydb"
	port:    3000
	debug:   true
	ratio:   0.5
	nothing: null
}`)
	settings := exactPackage(moduleRoot).instance(t, ".")["settings"].(map[string]interface{})
	if settings["url"] != "postgres://localhost/mydb" {
		t.Errorf("unexpected url: %v", settings["url"])
	}
	if port, ok := settings["port"].(float64); !ok || port != 3000 {
		t.Errorf("expected port 3000, got %v (%T)", settings["port"], settings["port"])
	}
	if debug, ok := settings["debug"].(bool); !ok || !debug {
		t.Errorf("expected debug true, got %v (%T)", settings["debug"], settings["debug"])
	}
	if ratio, ok := settings["ratio"].(float64); !ok || ratio != 0.5 {
		t.Errorf("expected ratio 0.5, got %v (%T)", settings["ratio"], settings["ratio"])
	}
	if nothing, present := settings["nothing"]; !present || nothing != nil {
		t.Errorf("expected nothing to export as null, got %v (present: %t)", nothing, present)
	}
}

func TestEvaluateModule_ComplexNestedStructure(t *testing.T) {
	moduleRoot := writePackage(t, `
settings: {
	database: {
		host: "localhost"
		port: 5432
	}
	tags: ["production", "web", "api"]
	empty: []
	matrix: [[1, 2], [3]]
}`)
	settings := exactPackage(moduleRoot).instance(t, ".")["settings"].(map[string]interface{})
	database, ok := settings["database"].(map[string]interface{})
	if !ok {
		t.Fatalf("expected database object, got %T", settings["database"])
	}
	if database["host"] != "localhost" {
		t.Errorf("unexpected database.host: %v", database["host"])
	}
	tags, ok := settings["tags"].([]interface{})
	if !ok || len(tags) != 3 || tags[0] != "production" || tags[2] != "api" {
		t.Errorf("unexpected tags: %v", settings["tags"])
	}
	if empty, ok := settings["empty"].([]interface{}); !ok || len(empty) != 0 {
		t.Errorf("expected empty to export as [], got %v (%T)", settings["empty"], settings["empty"])
	}
	matrix, ok := settings["matrix"].([]interface{})
	if !ok || len(matrix) != 2 {
		t.Fatalf("unexpected matrix: %v", settings["matrix"])
	}
	if first, ok := matrix[0].([]interface{}); !ok || len(first) != 2 {
		t.Errorf("unexpected matrix[0]: %v", matrix[0])
	}
}

func TestEvaluateModule_InvalidCueSyntax(t *testing.T) {
	moduleRoot := writePackage(t, `
settings: {
	unclosed: "missing closing brace"
`)
	exactPackage(moduleRoot).failure(t)
}

func TestEvaluateModule_WrongPackageNameIsAbsent(t *testing.T) {
	// A package filter is a presence query: another package in the target
	// directory is an empty success, not an error.
	moduleRoot := writeCueModule(t, map[string]string{
		"values.cue": "package other\n\nvalue: \"value\"\n",
	})
	result := exactPackage(moduleRoot).result(t)
	if len(result.Instances) != 0 {
		t.Errorf("expected no instances, got %v", instanceNames(result))
	}
}

// Field ordering and determinism -----------------------------------------------

func TestFieldOrderingIsSortedAndStable(t *testing.T) {
	// The bridge exports struct fields through Go maps, which encoding/json
	// writes in sorted key order, and the Rust side decodes into
	// serde_json::Value without preserve_order. Declaration order is
	// therefore not part of the contract; sorted, byte-stable output is.
	moduleRoot := writePackage(t, `
ordered: {
	zebra:  {value: "zebra"}
	alpha:  {value: "alpha"}
	omega:  {value: "omega"}
	beta:   {value: "beta"}
	nested: {third: 3, first: 1, second: 2}
}`)
	raw := string(exactPackage(moduleRoot).rawInstance(t, "."))
	assertKeyOrder(t, raw, `"alpha":`, `"beta":`, `"nested":`, `"omega":`, `"zebra":`)
	assertKeyOrder(t, raw, `"first":`, `"second":`, `"third":`)
}

func assertKeyOrder(t *testing.T, raw string, keys ...string) {
	t.Helper()
	previous := -1
	for _, key := range keys {
		position := strings.Index(raw, key)
		if position == -1 {
			t.Fatalf("key %s not found in %s", key, raw)
		}
		if position <= previous {
			t.Errorf("key %s is out of order in %s", key, raw)
		}
		previous = position
	}
}

func TestEvaluateModule_RepeatedCallsAreConsistent(t *testing.T) {
	moduleRoot := writePackage(t, `
ordered: {
	zebra: {value: "zebra"}
	alpha: {value: "alpha"}
	omega: {value: "omega"}
}
settings: value: "value"`)
	options := exactPackage(moduleRoot).optionsJSON(t)
	first := evaluateModuleResponse(moduleRoot, "", options)
	decodeEnvelope(t, first)
	for iteration := 0; iteration < 10; iteration++ {
		if next := evaluateModuleResponse(moduleRoot, "", options); next != first {
			t.Fatalf("iteration %d differs:\nfirst: %s\nnext:  %s", iteration, first, next)
		}
	}
}

func TestEvaluateModule_ConcurrentAccess(t *testing.T) {
	moduleRoot := writePackage(t, `settings: concurrent: "test"`)
	options := exactPackage(moduleRoot).optionsJSON(t)
	const goroutineCount = 5
	responses := make(chan string, goroutineCount)
	for index := 0; index < goroutineCount; index++ {
		go func() {
			responses <- evaluateModuleResponse(moduleRoot, "", options)
		}()
	}
	for index := 0; index < goroutineCount; index++ {
		response := <-responses
		assertContains(t, response, `"concurrent":"test"`)
	}
}

// Source metadata --------------------------------------------------------------

func TestSourceMetadata(t *testing.T) {
	// withMeta reports where every field is declared, including nested
	// fields, keyed as "<instance>/<field path>".
	moduleRoot := writeCueModule(t, map[string]string{
		"values.cue": packageSource(`
groups: {
	build: {
		command: "cargo"
	}
	nested: {
		children: {
			first: {command: "echo"}
		}
	}
}`),
		"child/values.cue": packageSource(`child: "value"`),
	})

	rootEvaluation := exactPackage(moduleRoot)
	rootEvaluation.withMeta = true
	meta := rootEvaluation.result(t).Meta
	expectations := map[string]int{
		"./groups":                       4,
		"./groups.build":                 5,
		"./groups.build.command":         6,
		"./groups.nested.children.first": 10,
	}
	for key, line := range expectations {
		entry, ok := meta[key]
		if !ok {
			t.Errorf("meta entry %s missing", key)
			continue
		}
		if entry.Filename != "values.cue" || entry.Directory != "." || entry.Line != line {
			t.Errorf("meta entry %s = %+v, want values.cue:%d in .", key, entry, line)
		}
	}

	childEvaluation := exactPackage(moduleRoot)
	childEvaluation.targetDirectory = "child"
	childEvaluation.withMeta = true
	childMeta := childEvaluation.result(t).Meta
	entry, ok := childMeta["child/child"]
	if !ok {
		t.Fatalf("meta entry child/child missing: %v", childMeta)
	}
	if entry.Filename != "child/values.cue" || entry.Directory != "child" || entry.Line != 3 {
		t.Errorf("unexpected child meta: %+v", entry)
	}

	// Without withMeta no metadata is returned.
	if withoutMeta := exactPackage(moduleRoot).result(t).Meta; len(withoutMeta) != 0 {
		t.Errorf("expected no meta without withMeta, got %d entries", len(withoutMeta))
	}
}

// Module dependencies ----------------------------------------------------------

func TestModuleDependencyVersion(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"cue.mod/module.cue": "module: \"example.com/bridge@v0\"\nlanguage: version: \"v0.14.1\"\n" +
			"deps: \"example.com/dependency@v0\": v: \"v0.53.1\"\n",
	})
	response := moduleDependencyVersionResponse(moduleRoot, "example.com/dependency")
	assertContains(t, response, `"ok":{"version":"v0.53.1"}`)

	absent := moduleDependencyVersionResponse(moduleRoot, "example.com/absent")
	assertContains(t, absent, `"ok":{"version":null}`)

	missing := moduleDependencyVersionResponse(t.TempDir(), "example.com/dependency")
	assertContains(t, missing, ErrorCodeInvalidInput)
}

// Concrete paths ---------------------------------------------------------------

// schemaPackage is a module-local package with a closed definition, standing
// in for any imported schema.
const schemaPackage = `package schema

#Empty: close({})

#Settings: close({
	size!:    int
	enabled?: bool
	labels?:  [string]: string
})
`

// importingPackage returns a package importing the local schema package as
// importName, with the given body.
func importingPackage(importName string, body string) string {
	return fmt.Sprintf("package %s\n\nimport %s \"%s/schema\"\n\n%s\n", testPackageName, importName, testModulePath, body)
}

// writeImportingModule writes the schema package and an importing package in
// the "consumer" directory.
func writeImportingModule(t *testing.T, importName string, body string) string {
	t.Helper()
	return writeCueModule(t, map[string]string{
		"schema/schema.cue":   schemaPackage,
		"consumer/values.cue": importingPackage(importName, body),
	})
}

// consumer evaluates the "consumer" directory with the given concrete paths.
func consumer(moduleRoot string, concretePaths ...string) evaluation {
	return evaluation{
		moduleRoot:      moduleRoot,
		targetDirectory: "consumer",
		packageName:     packageNamed(testPackageName),
		concretePaths:   concretePaths,
	}
}

func TestConcretePaths_ShadowedImportIsReported(t *testing.T) {
	// `schema` inside `items: schema: {...}` resolves to that field, not the
	// imported package, so `schema.#Empty` is undefined.
	moduleRoot := writeImportingModule(t, "schema", `
config: {
	primary: schema.#Settings & {size: 1}
	items: schema: {
		kind:     "first"
		settings: schema.#Empty
	}
}`)
	failure := consumer(moduleRoot, "config").failureText(t)
	assertContains(t, failure,
		"consumer: config:",
		"config.items.schema.settings",
		"undefined field: #Empty",
		"values.cue:",
	)
}

func TestConcretePaths_MissingRequiredFieldIsReported(t *testing.T) {
	moduleRoot := writeImportingModule(t, "schemaPackage", `
config: primary: schemaPackage.#Settings & {enabled: false}`)
	failure := consumer(moduleRoot, "config").failureText(t)
	assertContains(t, failure,
		"consumer: config:",
		"config.primary.size",
		"field is required but not present",
	)
}

func TestConcretePaths_NonConcreteValueIsReported(t *testing.T) {
	moduleRoot := writeImportingModule(t, "schemaPackage", `
config: primary: schemaPackage.#Settings & {
	size:    16
	enabled: bool
}`)
	failure := consumer(moduleRoot, "config").failureText(t)
	assertContains(t, failure,
		"consumer: config:",
		"config.primary.enabled",
		"incomplete value bool",
	)
}

func TestConcretePaths_ConcreteValueIsExportedUnchanged(t *testing.T) {
	moduleRoot := writeImportingModule(t, "schemaPackage", `
config: {
	name:  string | *"default"
	empty: schemaPackage.#Empty
	primary: schemaPackage.#Settings & {
		size:    16
		enabled: false
		labels: team: "platform"
	}
	order: []
}`)
	value := consumer(moduleRoot, "config").instance(t, "consumer")
	exported, err := json.Marshal(value["config"])
	if err != nil {
		t.Fatal(err)
	}
	expected := `{"empty":{},"name":"default","order":[],` +
		`"primary":{"enabled":false,"labels":{"team":"platform"},"size":16}}`
	if string(exported) != expected {
		t.Errorf("unexpected export:\n got: %s\nwant: %s", exported, expected)
	}
}

func TestConcretePaths_OnlyNamedPathsAreValidated(t *testing.T) {
	// Validation is scoped to the named paths: elsewhere a non-concrete
	// value and a missing required field still export as null.
	moduleRoot := writeImportingModule(t, "schemaPackage", `
config: size: 1
loose: {
	port: int
	host: "localhost"
}
settings: schemaPackage.#Settings & {enabled: true}`)
	value := consumer(moduleRoot, "config").instance(t, "consumer")
	loose := value["loose"].(map[string]interface{})
	if port, present := loose["port"]; !present || port != nil {
		t.Errorf("expected port to export as null, got %v (present: %t)", port, present)
	}
	if loose["host"] != "localhost" {
		t.Errorf("unexpected host: %v", loose["host"])
	}
	settings := value["settings"].(map[string]interface{})
	if size, present := settings["size"]; !present || size != nil {
		t.Errorf("expected missing required field to export as null, got %v", settings)
	}
}

func TestConcretePaths_AreOptIn(t *testing.T) {
	// Without concretePaths the bridge keeps its lenient export.
	moduleRoot := writeImportingModule(t, "schemaPackage", `
config: primary: schemaPackage.#Settings & {enabled: false}`)
	value := consumer(moduleRoot).instance(t, "consumer")
	primary := value["config"].(map[string]interface{})["primary"].(map[string]interface{})
	if size, present := primary["size"]; !present || size != nil {
		t.Errorf("expected lenient null export, got %v", primary)
	}
}

func TestConcretePaths_NestedPathValidatesOnlyThatSubtree(t *testing.T) {
	moduleRoot := writeImportingModule(t, "schemaPackage", `
config: {
	strict: primary: schemaPackage.#Settings & {size: int}
	lenient: port: int
}`)
	failure := consumer(moduleRoot, "config.strict").failureText(t)
	assertContains(t, failure, "consumer: config.strict:", "config.strict.primary.size", "incomplete value int")

	// The non-concrete sibling outside the named subtree does not fail.
	valid := writeImportingModule(t, "schemaPackage", `
config: {
	strict: primary: schemaPackage.#Settings & {size: 1}
	lenient: port: int
}`)
	value := consumer(valid, "config.strict").instance(t, "consumer")
	lenient := value["config"].(map[string]interface{})["lenient"].(map[string]interface{})
	if port, present := lenient["port"]; !present || port != nil {
		t.Errorf("expected lenient sibling to export as null, got %v", lenient)
	}
}

func TestConcretePaths_QuotedAndIndexedPaths(t *testing.T) {
	moduleRoot := writeImportingModule(t, "schemaPackage", `
"my-config": items: [
	{size: 1},
	schemaPackage.#Settings & {enabled: true},
]`)
	consumer(moduleRoot, `"my-config".items[0]`).instance(t, "consumer")
	failure := consumer(moduleRoot, `"my-config".items[1]`).failureText(t)
	assertContains(t, failure, `"my-config".items[1]`, "field is required but not present")
}

func TestConcretePaths_MissingPathFailsClosed(t *testing.T) {
	moduleRoot := writeImportingModule(t, "schemaPackage", `config: schemaPackage.#Settings & {size: 1}`)
	for _, path := range []string{"absent", "config.absent", "config.size.deeper"} {
		failure := consumer(moduleRoot, path).failureText(t)
		assertContains(t, failure, "consumer: "+path+": concrete path does not exist")
	}
}

func TestConcretePaths_MalformedPathIsInvalidInput(t *testing.T) {
	moduleRoot := writeImportingModule(t, "schemaPackage", `config: schemaPackage.#Settings & {size: 1}`)
	for _, path := range []string{"", "   ", "config..size", "config[", "config.", "1config"} {
		bridgeError := consumer(moduleRoot, path).failure(t)
		assertErrorCode(t, bridgeError, ErrorCodeInvalidInput)
		assertContains(t, bridgeError.Message, "concrete path")
	}
}

// Instance failures ------------------------------------------------------------

// writeMixedModule writes a module with two instances of the test package:
// "valid" evaluates and "broken" fails to build.
func writeMixedModule(t *testing.T) string {
	t.Helper()
	return writeCueModule(t, map[string]string{
		"valid/values.cue":  packageSource(`config: size: 1`),
		"broken/values.cue": packageSource(`config: size: 1 & 2`),
		"other/values.cue":  "package other\n\nconfig: size: \"ignored\"\n",
	})
}

func TestInstanceFailures_AreSkippedByDefault(t *testing.T) {
	// With the default skip policy an instance that fails to build is left
	// out of a recursive result as long as another instance succeeds.
	moduleRoot := writeMixedModule(t)
	result := evaluation{moduleRoot: moduleRoot, recursive: true, packageName: packageNamed(testPackageName)}.result(t)
	if _, ok := result.Instances["valid"]; !ok || len(result.Instances) != 1 {
		t.Errorf("expected only the valid instance, got %v", instanceNames(result))
	}
}

func TestInstanceFailures_FailPolicyNamesEveryFailure(t *testing.T) {
	moduleRoot := writeMixedModule(t)
	bridgeError := evaluation{
		moduleRoot:       moduleRoot,
		recursive:        true,
		packageName:      packageNamed(testPackageName),
		instanceFailures: InstanceFailuresFail,
	}.failure(t)
	assertErrorCode(t, bridgeError, ErrorCodeBuildValue)
	assertContains(t, bridgeError.Message, "1 instance(s) could not be evaluated", "broken: config.size: conflicting values")
	if strings.Contains(bridgeError.Message, "valid:") || strings.Contains(bridgeError.Message, "other:") {
		t.Errorf("only failed instances should be named: %s", bridgeError.Message)
	}
}

func TestInstanceFailures_FailPolicyIncludesConcretePathFailures(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"complete/values.cue":   packageSource(`config: size: 1`),
		"incomplete/values.cue": packageSource(`config: size: int`),
		"missing/values.cue":    packageSource(`other: 1`),
	})
	base := evaluation{
		moduleRoot:    moduleRoot,
		recursive:     true,
		packageName:   packageNamed(testPackageName),
		concretePaths: []string{"config"},
	}
	result := base.result(t)
	if _, ok := result.Instances["complete"]; !ok || len(result.Instances) != 1 {
		t.Errorf("expected only the complete instance, got %v", instanceNames(result))
	}

	strict := base
	strict.instanceFailures = InstanceFailuresFail
	bridgeError := strict.failure(t)
	assertContains(t, bridgeError.Message,
		"2 instance(s) could not be evaluated",
		"incomplete: config: config.size: incomplete value int",
		"missing: config: concrete path does not exist",
	)
	if strings.Index(bridgeError.Message, "incomplete:") > strings.Index(bridgeError.Message, "missing:") {
		t.Errorf("failures should be sorted by instance: %s", bridgeError.Message)
	}
}

func TestInstanceFailures_FailPolicyAcceptsCleanModule(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"first/values.cue":  packageSource(`config: size: 1`),
		"second/values.cue": packageSource(`config: size: 2`),
	})
	result := evaluation{
		moduleRoot:       moduleRoot,
		recursive:        true,
		packageName:      packageNamed(testPackageName),
		instanceFailures: InstanceFailuresFail,
	}.result(t)
	if len(result.Instances) != 2 {
		t.Errorf("expected two instances, got %v", instanceNames(result))
	}
}

func TestInstanceFailures_ExplicitSkipPolicyMatchesDefault(t *testing.T) {
	moduleRoot := writeMixedModule(t)
	result := evaluation{
		moduleRoot:       moduleRoot,
		recursive:        true,
		packageName:      packageNamed(testPackageName),
		instanceFailures: InstanceFailuresSkip,
	}.result(t)
	if _, ok := result.Instances["valid"]; !ok || len(result.Instances) != 1 {
		t.Errorf("expected only the valid instance, got %v", instanceNames(result))
	}
}

func TestInstanceFailures_UnknownPolicyIsInvalidInput(t *testing.T) {
	moduleRoot := writeMixedModule(t)
	bridgeError := evaluation{
		moduleRoot:       moduleRoot,
		recursive:        true,
		packageName:      packageNamed(testPackageName),
		instanceFailures: "ignore",
	}.failure(t)
	assertErrorCode(t, bridgeError, ErrorCodeInvalidInput)
	assertContains(t, bridgeError.Message, `Unknown instanceFailures value "ignore"`)
}

func TestUnfilteredRecursiveEvaluation(t *testing.T) {
	// Without a package filter a recursive load evaluates the single package
	// of every directory, whatever it is called.
	moduleRoot := writeCueModule(t, map[string]string{
		"first/values.cue":  "package first\n\nname: \"one\"\n",
		"second/values.cue": "package second\n\nname: \"two\"\n",
	})
	result := evaluation{moduleRoot: moduleRoot, recursive: true, instanceFailures: InstanceFailuresFail}.result(t)
	if len(result.Instances) != 2 {
		t.Errorf("expected both packages, got %v", instanceNames(result))
	}

	// A directory holding two packages is a loader-level failure with no
	// instance directory; it is named by the load pattern.
	mixed := writeCueModule(t, map[string]string{
		"first/values.cue": "package first\n\nname: \"one\"\n",
		"mixed/a.cue":      "package alpha\n\nname: \"alpha\"\n",
		"mixed/b.cue":      "package beta\n\nname: \"beta\"\n",
	})
	bridgeError := evaluation{moduleRoot: mixed, recursive: true, instanceFailures: InstanceFailuresFail}.failure(t)
	assertContains(t, bridgeError.Message, `./...: found packages "alpha"`)
}
