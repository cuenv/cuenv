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
// not allow cgo in test files.

const testModulePath = "example.com/bridge"

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

// writeProject creates a module whose root holds a single `cuenv` package.
func writeProject(t *testing.T, contents string) string {
	t.Helper()
	return writeCueModule(t, map[string]string{"env.cue": "package cuenv\n\n" + contents})
}

// testEnvelope mirrors BridgeResponse with the success payload decoded.
type testEnvelope struct {
	Version string          `json:"version"`
	Ok      json.RawMessage `json:"ok"`
	Error   *BridgeError    `json:"error"`
}

// evaluateModule runs a non-recursive evaluation of the `cuenv` package in
// targetDirectory (relative to moduleRoot), requiring `infrastructure` to be
// concrete as cuenv does, and decodes the envelope.
func evaluateModule(t *testing.T, moduleRoot string, targetDirectory string) testEnvelope {
	t.Helper()
	return evaluateModuleWith(t, moduleRoot, targetDirectory, []string{"infrastructure"})
}

// evaluateModuleWith is evaluateModule with explicit concrete paths.
func evaluateModuleWith(t *testing.T, moduleRoot string, targetDirectory string, concretePaths []string) testEnvelope {
	t.Helper()
	options, err := json.Marshal(map[string]interface{}{
		"packageName":   "cuenv",
		"targetDir":     filepath.Join(moduleRoot, filepath.FromSlash(targetDirectory)),
		"concretePaths": concretePaths,
	})
	if err != nil {
		t.Fatalf("marshal options: %v", err)
	}
	response := evaluateModuleResponse(moduleRoot, "", string(options))
	var envelope testEnvelope
	if err := json.Unmarshal([]byte(response), &envelope); err != nil {
		t.Fatalf("parse bridge response: %v\nresponse: %s", err, response)
	}
	if envelope.Version != BridgeVersion {
		t.Fatalf("expected bridge version %q, got %q", BridgeVersion, envelope.Version)
	}
	return envelope
}

// evaluateInstance evaluates the module and returns the JSON of the instance
// at targetDirectory, failing the test on a bridge error.
func evaluateInstance(t *testing.T, moduleRoot string, targetDirectory string) map[string]interface{} {
	t.Helper()
	envelope := evaluateModule(t, moduleRoot, targetDirectory)
	if envelope.Error != nil {
		t.Fatalf("unexpected bridge error: %+v (hint: %s)", envelope.Error, errorHint(envelope.Error))
	}
	var result ModuleResult
	if err := json.Unmarshal(envelope.Ok, &result); err != nil {
		t.Fatalf("parse module result: %v\nresult: %s", err, envelope.Ok)
	}
	instance, ok := result.Instances[targetDirectory]
	if !ok {
		t.Fatalf("instance %q missing from result: %s", targetDirectory, envelope.Ok)
	}
	var value map[string]interface{}
	if err := json.Unmarshal(instance, &value); err != nil {
		t.Fatalf("parse instance: %v\ninstance: %s", err, instance)
	}
	return value
}

// evaluateFailure evaluates the module and returns the bridge error message
// and hint, failing the test when evaluation succeeds.
func evaluateFailure(t *testing.T, moduleRoot string, targetDirectory string) string {
	t.Helper()
	envelope := evaluateModule(t, moduleRoot, targetDirectory)
	if envelope.Error == nil {
		t.Fatalf("expected a bridge error, got: %s", envelope.Ok)
	}
	failure := envelope.Error.Message + "\n" + errorHint(envelope.Error)
	t.Logf("bridge error: %s", failure)
	return failure
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

func TestEvaluateModule_ValidInput(t *testing.T) {
	moduleRoot := writeProject(t, `
env: {
	DATABASE_URL: "postgres://localhost/mydb"
	PORT: 3000
	DEBUG: true
}`)
	value := evaluateInstance(t, moduleRoot, ".")
	env, ok := value["env"].(map[string]interface{})
	if !ok {
		t.Fatalf("expected env object, got %T", value["env"])
	}
	if env["DATABASE_URL"] != "postgres://localhost/mydb" {
		t.Errorf("unexpected DATABASE_URL: %v", env["DATABASE_URL"])
	}
	if port, ok := env["PORT"].(float64); !ok || port != 3000 {
		t.Errorf("expected PORT 3000, got %v (%T)", env["PORT"], env["PORT"])
	}
	if debug, ok := env["DEBUG"].(bool); !ok || !debug {
		t.Errorf("expected DEBUG true, got %v (%T)", env["DEBUG"], env["DEBUG"])
	}
}

func TestEvaluateModule_ComplexNestedStructure(t *testing.T) {
	moduleRoot := writeProject(t, `
env: {
	DATABASE: {
		HOST: "localhost"
		PORT: 5432
	}
	TAGS: ["production", "web", "api"]
	EMPTY: []
}`)
	value := evaluateInstance(t, moduleRoot, ".")
	env := value["env"].(map[string]interface{})
	database, ok := env["DATABASE"].(map[string]interface{})
	if !ok {
		t.Fatalf("expected DATABASE object, got %T", env["DATABASE"])
	}
	if database["HOST"] != "localhost" {
		t.Errorf("unexpected DATABASE.HOST: %v", database["HOST"])
	}
	tags, ok := env["TAGS"].([]interface{})
	if !ok || len(tags) != 3 || tags[0] != "production" {
		t.Errorf("unexpected TAGS: %v", env["TAGS"])
	}
	if empty, ok := env["EMPTY"].([]interface{}); !ok || len(empty) != 0 {
		t.Errorf("expected EMPTY to export as [], got %v (%T)", env["EMPTY"], env["EMPTY"])
	}
}

func TestEvaluateModule_EmptyModuleRoot(t *testing.T) {
	var envelope testEnvelope
	if err := json.Unmarshal([]byte(evaluateModuleResponse("", "cuenv", "")), &envelope); err != nil {
		t.Fatalf("parse bridge response: %v", err)
	}
	if envelope.Error == nil || envelope.Error.Code != ErrorCodeInvalidInput {
		t.Fatalf("expected %s error, got %+v", ErrorCodeInvalidInput, envelope.Error)
	}
	assertContains(t, envelope.Error.Message, "Module root path cannot be empty")
}

func TestEvaluateModule_InvalidOptions(t *testing.T) {
	var envelope testEnvelope
	if err := json.Unmarshal([]byte(evaluateModuleResponse(t.TempDir(), "cuenv", "{")), &envelope); err != nil {
		t.Fatalf("parse bridge response: %v", err)
	}
	if envelope.Error == nil || envelope.Error.Code != ErrorCodeInvalidInput {
		t.Fatalf("expected %s error, got %+v", ErrorCodeInvalidInput, envelope.Error)
	}
}

func TestEvaluateModule_InvalidCueSyntax(t *testing.T) {
	moduleRoot := writeProject(t, `
env: {
	INVALID_SYNTAX: "missing closing brace"
`)
	envelope := evaluateModule(t, moduleRoot, ".")
	if envelope.Error == nil {
		t.Fatalf("expected a syntax error, got: %s", envelope.Ok)
	}
}

func TestEvaluateModule_WrongPackageNameIsAbsent(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"env.cue": "package other\n\nenv: TEST_VAR: \"value\"\n",
	})
	envelope := evaluateModule(t, moduleRoot, ".")
	if envelope.Error != nil {
		t.Fatalf("expected empty success for a package mismatch, got %+v", envelope.Error)
	}
	var result ModuleResult
	if err := json.Unmarshal(envelope.Ok, &result); err != nil {
		t.Fatalf("parse module result: %v", err)
	}
	if len(result.Instances) != 0 {
		t.Errorf("expected no instances, got %s", envelope.Ok)
	}
}

func TestEvaluateModule_RepeatedCallsAreConsistent(t *testing.T) {
	moduleRoot := writeProject(t, `
tasks: {
	zebra: { command: "echo zebra" }
	alpha: { command: "echo alpha" }
}
env: TEST_VAR: "value"`)
	options := fmt.Sprintf(`{"packageName":"cuenv","targetDir":%q}`, moduleRoot)
	first := evaluateModuleResponse(moduleRoot, "", options)
	for iteration := 0; iteration < 5; iteration++ {
		if next := evaluateModuleResponse(moduleRoot, "", options); next != first {
			t.Fatalf("iteration %d differs:\nfirst: %s\nnext:  %s", iteration, first, next)
		}
	}
}

func TestEvaluateModule_ConcurrentAccess(t *testing.T) {
	moduleRoot := writeProject(t, `env: CONCURRENT_VAR: "test"`)
	options := fmt.Sprintf(`{"packageName":"cuenv","targetDir":%q}`, moduleRoot)
	const goroutineCount = 5
	responses := make(chan string, goroutineCount)
	for index := 0; index < goroutineCount; index++ {
		go func() {
			responses <- evaluateModuleResponse(moduleRoot, "", options)
		}()
	}
	for index := 0; index < goroutineCount; index++ {
		response := <-responses
		assertContains(t, response, `"CONCURRENT_VAR":"test"`)
	}
}

func TestModuleDependencyVersion(t *testing.T) {
	moduleRoot := t.TempDir()
	moduleFile := filepath.Join(moduleRoot, "cue.mod", "module.cue")
	if err := os.MkdirAll(filepath.Dir(moduleFile), 0o755); err != nil {
		t.Fatal(err)
	}
	contents := "module: \"example.com/bridge@v0\"\nlanguage: version: \"v0.14.1\"\n" +
		"deps: \"github.com/cuenv/cuenv@v0\": v: \"v0.53.1\"\n"
	if err := os.WriteFile(moduleFile, []byte(contents), 0o644); err != nil {
		t.Fatal(err)
	}
	response := moduleDependencyVersionResponse(moduleRoot, "github.com/cuenv/cuenv")
	assertContains(t, response, `"ok":{"version":"v0.53.1"}`)

	missing := moduleDependencyVersionResponse(t.TempDir(), "github.com/cuenv/cuenv")
	assertContains(t, missing, ErrorCodeInvalidInput)
}

// Infrastructure concreteness ------------------------------------------------

// randomProviderPackage stands in for a github.com/cuenv/terraform provider
// module: typed, closed provider and resource definitions.
const randomProviderPackage = `package random

#ProviderConfig: close({})

#Resource_random_password: close({
	length!:  int
	special?: bool
	upper?:   bool
})
`

// infrastructureProject returns a project importing the local random provider
// package as importName with the given infrastructure block body.
func infrastructureProject(importName string, infrastructure string) string {
	return fmt.Sprintf(`package cuenv

import %s "%s/random"

name: "infrastructure-project"

infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"
%s
}
`, importName, testModulePath, infrastructure)
}

func writeInfrastructureModule(t *testing.T, importName string, infrastructure string) string {
	t.Helper()
	return writeCueModule(t, map[string]string{
		"random/random.cue": randomProviderPackage,
		"app/env.cue":       infrastructureProject(importName, infrastructure),
	})
}

func TestInfrastructure_ShadowedImportIsReported(t *testing.T) {
	// `random` inside `providers: random: {...}` resolves to that field, not
	// the imported package, so `random.#ProviderConfig` is undefined.
	moduleRoot := writeInfrastructureModule(t, "random", `
	providers: random: {
		source:        "hashicorp/random"
		version:       "3.7.2"
		configuration: random.#ProviderConfig
	}
	resources: password: {
		type:          "random_password"
		configuration: random.#Resource_random_password & {length: 16}
	}`)
	failure := evaluateFailure(t, moduleRoot, "app")
	assertContains(t, failure,
		"app: infrastructure:",
		"infrastructure.providers.random.configuration",
		"undefined field: #ProviderConfig",
		"env.cue:",
	)
}

func TestInfrastructure_MissingRequiredFieldIsReported(t *testing.T) {
	moduleRoot := writeInfrastructureModule(t, "randomProvider", `
	resources: password: {
		type:          "random_password"
		configuration: randomProvider.#Resource_random_password & {special: false}
	}`)
	failure := evaluateFailure(t, moduleRoot, "app")
	assertContains(t, failure,
		"app: infrastructure:",
		"infrastructure.resources.password.configuration.length",
		"field is required but not present",
	)
}

func TestInfrastructure_NonConcreteValueIsReported(t *testing.T) {
	moduleRoot := writeInfrastructureModule(t, "randomProvider", `
	resources: password: {
		type:          "random_password"
		configuration: randomProvider.#Resource_random_password & {
			length:  16
			special: bool
		}
	}`)
	failure := evaluateFailure(t, moduleRoot, "app")
	assertContains(t, failure,
		"app: infrastructure:",
		"infrastructure.resources.password.configuration.special",
		"incomplete value bool",
	)
}

func TestInfrastructure_ConcreteBlockIsExportedUnchanged(t *testing.T) {
	moduleRoot := writeInfrastructureModule(t, "randomProvider", `
	state: turso: authenticationTokenEnvironmentVariable: string | *"TURSO_AUTH_TOKEN"
	providers: random: {
		source:        "hashicorp/random"
		version:       "3.7.2"
		configuration: randomProvider.#ProviderConfig
	}
	resources: password: {
		type:          "random_password"
		dependsOn: []
		configuration: randomProvider.#Resource_random_password & {
			length:  16
			special: false
		}
	}`)
	value := evaluateInstance(t, moduleRoot, "app")
	exported, err := json.Marshal(value["infrastructure"])
	if err != nil {
		t.Fatal(err)
	}
	expected := `{"providers":{"random":{"configuration":{},"source":"hashicorp/random","version":"3.7.2"}},` +
		`"resources":{"password":{"configuration":{"length":16,"special":false},"dependsOn":[],"type":"random_password"}},` +
		`"state":{"turso":{"authenticationTokenEnvironmentVariable":"TURSO_AUTH_TOKEN","url":"http://127.0.0.1:8080"}}}`
	if string(exported) != expected {
		t.Errorf("unexpected infrastructure export:\n got: %s\nwant: %s", exported, expected)
	}
}

func TestInfrastructure_ProjectsWithoutInfrastructureKeepLenientExport(t *testing.T) {
	// Validation is scoped to `infrastructure`: elsewhere a non-concrete
	// value and a missing required field still export as null, exactly as
	// before.
	moduleRoot := writeProject(t, `
#Settings: close({
	required!: string
	optional?: int
})

name: "plain-project"
env: {
	PORT:  int
	HOST:  "localhost"
}
settings: #Settings & {optional: 1}`)
	value := evaluateInstance(t, moduleRoot, ".")
	env := value["env"].(map[string]interface{})
	port, present := env["PORT"]
	if !present || port != nil {
		t.Errorf("expected PORT to export as null, got %v (present: %t)", port, present)
	}
	if env["HOST"] != "localhost" {
		t.Errorf("unexpected HOST: %v", env["HOST"])
	}
	settings := value["settings"].(map[string]interface{})
	if required, present := settings["required"]; !present || required != nil {
		t.Errorf("expected missing required field to export as null, got %v", settings)
	}
	if settings["optional"] != float64(1) {
		t.Errorf("unexpected settings.optional: %v", settings["optional"])
	}
}

func TestConcretePaths_AreOptIn(t *testing.T) {
	// Without concretePaths the bridge keeps its lenient export, so the check
	// only applies where a caller asks for it.
	moduleRoot := writeInfrastructureModule(t, "randomProvider", `
	resources: password: {
		type:          "random_password"
		configuration: randomProvider.#Resource_random_password & {special: false}
	}`)
	envelope := evaluateModuleWith(t, moduleRoot, "app", nil)
	if envelope.Error != nil {
		t.Fatalf("expected lenient export without concretePaths, got error: %+v", envelope.Error)
	}
}
