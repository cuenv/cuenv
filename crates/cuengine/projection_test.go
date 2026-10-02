package main

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"
)

func writeProjectionModule(t *testing.T) string {
	t.Helper()
	schema := "#Project: {name!: string, infrastructure?: {url: string}, config?: {...}, tasks?: {...}}\n"
	return writeCueModule(t, map[string]string{
		"a/values.cue": packageSource(schema + `#Project & {
	name: "a"
	infrastructure: url: "libsql://a"
	config: {database: {host: "db", port: 5432}, cache: true}
	tasks: build: command: "make"
}`),
		"b/values.cue": packageSource(schema + `#Project & {name: "b"}`),
	})
}

func decodeInstances(t *testing.T, result ModuleResult) map[string]interface{} {
	t.Helper()
	decoded := make(map[string]interface{})
	for key, raw := range result.Instances {
		var value interface{}
		if err := json.Unmarshal(raw, &value); err != nil {
			t.Fatalf("parse instance %s: %v", key, err)
		}
		decoded[key] = value
	}
	return decoded
}

func TestExportPaths_ExportOnlyTheNamedFields(t *testing.T) {
	moduleRoot := writeProjectionModule(t)
	projected := allPackages(moduleRoot)
	projected.exportPaths = []string{"name", "config.database.host", "missing.field"}
	result := projected.result(t)

	expected := map[string]interface{}{
		"a:app": map[string]interface{}{
			"name":   "a",
			"config": map[string]interface{}{"database": map[string]interface{}{"host": "db"}},
		},
		"b:app": map[string]interface{}{"name": "b"},
	}
	if got := decodeInstances(t, result); !reflect.DeepEqual(got, expected) {
		t.Errorf("expected %v, got %v", expected, got)
	}
	assertContains(t, strings.Join(result.Projects, ","), "a:app", "b:app")
}

func TestExportPaths_WholeFieldWinsOverPathBelowIt(t *testing.T) {
	moduleRoot := writeProjectionModule(t)
	projected := allPackages(moduleRoot)
	projected.exportPaths = []string{"config.database.host", "config"}
	instances := decodeInstances(t, projected.result(t))
	config := instances["a:app"].(map[string]interface{})["config"].(map[string]interface{})
	if config["cache"] != true || config["database"].(map[string]interface{})["port"] != float64(5432) {
		t.Errorf("expected the whole config block, got %v", config)
	}
}

func TestPresencePaths_ReportRegularFieldsWithoutExportingThem(t *testing.T) {
	moduleRoot := writeProjectionModule(t)
	projected := allPackages(moduleRoot)
	projected.exportPaths = []string{"name"}
	projected.presencePaths = []string{"infrastructure", "tasks.build", "name"}
	result := projected.result(t)

	expected := map[string][]string{
		// A field only declared optional (infrastructure?) does not exist.
		"a:app": {"infrastructure", "tasks.build", "name"},
		"b:app": {"name"},
	}
	if !reflect.DeepEqual(result.Present, expected) {
		t.Errorf("expected presence %v, got %v", expected, result.Present)
	}
	if strings.Contains(string(result.Instances["a:app"]), "libsql") {
		t.Errorf("presence paths must not be exported: %s", result.Instances["a:app"])
	}

	// Without presence paths the result has no presence map.
	if present := allPackages(moduleRoot).result(t).Present; present != nil {
		t.Errorf("expected no presence map, got %v", present)
	}
}

func TestExportPaths_FailuresOutsideTheProjectionAreStillReported(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"good/values.cue":   packageSource(`name: "good"`),
		"broken/values.cue": packageSource("name: \"broken\"\ntasks: build: 1 & 2"),
	})
	strict := allPackages(moduleRoot)
	strict.exportPaths = []string{"name"}
	strict.instanceFailures = InstanceFailuresFail
	bridgeError := strict.failure(t)
	assertContains(t, bridgeError.Message, "broken:app: tasks.build: conflicting values")
}

func TestExportPaths_OnlyRegularFieldsAreAccepted(t *testing.T) {
	moduleRoot := writeProjectionModule(t)
	for _, option := range []string{"exportPaths", "presencePaths"} {
		for _, path := range []string{"", "tasks[0]", "#Project", "_hidden", "a.#b", "a b"} {
			invalid := allPackages(moduleRoot)
			if option == "exportPaths" {
				invalid.exportPaths = []string{path}
			} else {
				invalid.presencePaths = []string{path}
			}
			bridgeError := invalid.failure(t)
			assertErrorCode(t, bridgeError, ErrorCodeInvalidInput)
			assertContains(t, bridgeError.Message, option)
		}
	}
}
