package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func writeFileFixture(t *testing.T, path, contents string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(contents), 0o644); err != nil {
		t.Fatal(err)
	}
}

func fileResponse(t *testing.T, path string) BridgeResponse {
	t.Helper()
	var response BridgeResponse
	if err := json.Unmarshal([]byte(evaluateFileResponse(path)), &response); err != nil {
		t.Fatal(err)
	}
	return response
}

func TestEvaluateFileUsesModuleImportsButNotNeighbouringPackages(t *testing.T) {
	root := t.TempDir()
	writeFileFixture(t, filepath.Join(root, "cue.mod/module.cue"), "module: \"example.org/test@v0\"\nlanguage: version: \"v0.14.0\"\n")
	writeFileFixture(t, filepath.Join(root, "shared/schema.cue"), "package shared\n#Rules: {answer: 42}\n")
	writeFileFixture(t, filepath.Join(root, "env.cue"), "package cuenv\nanswer: 0\n")
	directory := filepath.Join(root, "nested")
	writeFileFixture(t, filepath.Join(directory, "env.cue"), "package cuenv\nanswer: 1\n")
	file := filepath.Join(directory, ".rules.cue")
	writeFileFixture(t, file, "package cuenv\nimport \"example.org/test/shared\"\nshared.#Rules\n")
	response := fileResponse(t, file)
	if response.Error != nil || response.Ok == nil || string(*response.Ok) != `{"answer":42}` {
		t.Fatalf("unexpected response: %+v", response)
	}
}

func TestEvaluateFileReportsSchemaFailureAtOriginalFile(t *testing.T) {
	root := t.TempDir()
	writeFileFixture(t, filepath.Join(root, "cue.mod/module.cue"), "module: \"example.org/test@v0\"\nlanguage: version: \"v0.14.0\"\n")
	writeFileFixture(t, filepath.Join(root, "shared/schema.cue"), "package shared\n#Rules: {answer: 42}\n")
	file := filepath.Join(root, ".rules.cue")
	writeFileFixture(t, file, "package cuenv\nimport \"example.org/test/shared\"\nshared.#Rules & {answer: 7}\n")
	response := fileResponse(t, file)
	if response.Error == nil || !strings.Contains(response.Error.Message, ".rules.cue") || !strings.Contains(response.Error.Message, "conflicting values") {
		t.Fatalf("expected schema error at original file, got %+v", response)
	}
}

func TestEvaluateFileOutsideModule(t *testing.T) {
	file := filepath.Join(t.TempDir(), ".rules.cue")
	writeFileFixture(t, file, "package cuenv\nanswer: 42\n")
	response := fileResponse(t, file)
	if response.Error != nil || response.Ok == nil || string(*response.Ok) != `{"answer":42}` {
		t.Fatalf("unexpected response: %+v", response)
	}
}

func TestEvaluateFileLoadsGeneratedPackages(t *testing.T) {
	root := t.TempDir()
	writeFileFixture(t, filepath.Join(root, "cue.mod/module.cue"), "module: \"example.org/test@v0\"\nlanguage: version: \"v0.14.0\"\n")
	writeFileFixture(t, filepath.Join(root, "cue.mod/gen/registry.terraform.io/example/provider/provider.cue"), "package provider\n#Config: {answer: 42}\n")
	file := filepath.Join(root, ".infrastructure.cue")
	writeFileFixture(t, file, "package cuenv\nimport \"registry.terraform.io/example/provider\"\nprovider.#Config\n")
	response := fileResponse(t, file)
	if response.Error != nil || response.Ok == nil || string(*response.Ok) != `{"answer":42}` {
		t.Fatalf("unexpected response: %+v", response)
	}
}

func TestEvaluateFileReportsMissingAndInvalidFiles(t *testing.T) {
	file := filepath.Join(t.TempDir(), ".rules.cue")
	if response := fileResponse(t, file); response.Error == nil {
		t.Fatalf("missing file unexpectedly succeeded: %+v", response)
	}
	writeFileFixture(t, file, "package cuenv\nvalue: {\n")
	response := fileResponse(t, file)
	if response.Error == nil || !strings.Contains(response.Error.Message, ".rules.cue") {
		t.Fatalf("expected error at original file, got %+v", response)
	}
}
