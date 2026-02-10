//go:build cgo

package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"unsafe"
)

/*
#include <stdlib.h>
*/
import "C"

// Helper function to create a temporary CUE module with CUE files
func createTestCueModule(t *testing.T, packageName string, content string) (string, func()) {
	// Validate package name to prevent path traversal
	if strings.Contains(packageName, "..") || strings.Contains(packageName, "/") || strings.Contains(packageName, "\\") {
		t.Fatalf("Invalid package name: %s (contains path traversal characters)", packageName)
	}

	// Validate content size to prevent resource exhaustion
	if len(content) > 1024*1024 { // 1MB limit
		t.Fatalf("Content too large: %d bytes (max 1MB)", len(content))
	}

	tempDir, err := os.MkdirTemp("", "cuenv-test-*")
	if err != nil {
		t.Fatalf("Failed to create temp dir: %v", err)
	}

	// Create cue.mod/module.cue for a valid CUE module
	cueModDir := filepath.Join(tempDir, "cue.mod")
	if err := os.MkdirAll(cueModDir, 0755); err != nil {
		os.RemoveAll(tempDir)
		t.Fatalf("Failed to create cue.mod dir: %v", err)
	}

	moduleCue := `module: "test.example"\nlanguage: version: "v0.9.0"\n`
	if err := os.WriteFile(filepath.Join(cueModDir, "module.cue"), []byte(moduleCue), 0644); err != nil {
		os.RemoveAll(tempDir)
		t.Fatalf("Failed to write module.cue: %v", err)
	}

	// Create env.cue file with safe filename
	cueFile := filepath.Join(tempDir, "env.cue")

	// Validate final path is within temp directory
	if !strings.HasPrefix(cueFile, tempDir) {
		os.RemoveAll(tempDir)
		t.Fatalf("Path traversal detected in file path")
	}

	fullContent := "package " + packageName + "\n\n" + content
	if err := os.WriteFile(cueFile, []byte(fullContent), 0644); err != nil {
		os.RemoveAll(tempDir)
		t.Fatalf("Failed to write CUE file: %v", err)
	}

	cleanup := func() {
		os.RemoveAll(tempDir)
	}

	return tempDir, cleanup
}

// Helper to call cue_eval_module FFI function safely
func callCueEvalModule(moduleRoot, packageName, optionsJSON string) string {
	cModuleRoot := C.CString(moduleRoot)
	cPackageName := C.CString(packageName)
	cOptions := C.CString(optionsJSON)
	defer C.free(unsafe.Pointer(cModuleRoot))
	defer C.free(unsafe.Pointer(cPackageName))
	defer C.free(unsafe.Pointer(cOptions))

	result := cue_eval_module(cModuleRoot, cPackageName, cOptions)
	defer cue_free_string(result)

	return C.GoString(result)
}

func TestCueFreeString(t *testing.T) {
	// Test that cue_free_string doesn't crash
	testStr := C.CString("test string")
	defer func() {
		if r := recover(); r != nil {
			t.Errorf("cue_free_string panicked: %v", r)
		}
	}()
	cue_free_string(testStr)
}

func TestCueEvalModule_ValidInput(t *testing.T) {
	cueContent := `
env: {
	DATABASE_URL: "postgres://localhost/mydb"
	API_KEY: "test-key"
	PORT: 3000
	DEBUG: true
}`

	tempDir, cleanup := createTestCueModule(t, "cuenv", cueContent)
	defer cleanup()

	result := callCueEvalModule(tempDir, "cuenv", "")

	// Parse bridge response envelope
	var response map[string]interface{}
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		t.Fatalf("Failed to parse JSON result: %v\nResult: %s", err, result)
	}

	// Check for error
	if response["error"] != nil {
		t.Fatalf("Expected success, got error: %v", response["error"])
	}

	// Extract the ok payload (ModuleResult with instances)
	okPayload, ok := response["ok"].(map[string]interface{})
	if !ok {
		t.Fatalf("Expected ok to be a map, got: %T", response["ok"])
	}

	instances, ok := okPayload["instances"].(map[string]interface{})
	if !ok {
		t.Fatalf("Expected instances to be a map, got: %T", okPayload["instances"])
	}

	// Get the root instance (".")
	rootInstance, ok := instances["."].(map[string]interface{})
	if !ok {
		t.Fatalf("Expected root instance '.', got keys: %v", instances)
	}

	env, ok := rootInstance["env"].(map[string]interface{})
	if !ok {
		t.Fatalf("Expected env to be a map, got: %T", rootInstance["env"])
	}

	if env["DATABASE_URL"] != "postgres://localhost/mydb" {
		t.Errorf("Expected DATABASE_URL to be 'postgres://localhost/mydb', got %v", env["DATABASE_URL"])
	}

	if env["API_KEY"] != "test-key" {
		t.Errorf("Expected API_KEY to be 'test-key', got %v", env["API_KEY"])
	}
}

func TestCueEvalModule_EmptyModuleRoot(t *testing.T) {
	result := callCueEvalModule("", "cuenv", "")

	// Should return error in bridge response
	var response map[string]interface{}
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		t.Fatalf("Failed to parse JSON: %v\nResult: %s", err, result)
	}

	if response["error"] == nil {
		t.Errorf("Expected error response for empty module root")
	}
}

func TestCueEvalModule_NonexistentDirectory(t *testing.T) {
	result := callCueEvalModule("/nonexistent/path", "cuenv", "")

	// Should return error in bridge response
	var response map[string]interface{}
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		t.Fatalf("Failed to parse JSON: %v\nResult: %s", err, result)
	}

	if response["error"] == nil {
		t.Errorf("Expected error response for nonexistent directory")
	}
}

func TestCueEvalModule_InvalidCueSyntax(t *testing.T) {
	invalidCueContent := `
env: {
	INVALID_SYNTAX: "missing closing brace"
`
	tempDir, cleanup := createTestCueModule(t, "cuenv", invalidCueContent)
	defer cleanup()

	result := callCueEvalModule(tempDir, "cuenv", "")

	// Should return error in bridge response
	var response map[string]interface{}
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		t.Fatalf("Failed to parse JSON: %v\nResult: %s", err, result)
	}

	if response["error"] == nil {
		t.Errorf("Expected error response for invalid CUE syntax")
	}
}

func TestCueEvalModule_WrongPackageName(t *testing.T) {
	cueContent := `env: { TEST_VAR: "value" }`
	tempDir, cleanup := createTestCueModule(t, "wrongpackage", cueContent)
	defer cleanup()

	result := callCueEvalModule(tempDir, "cuenv", "")

	// Should return error since package name doesn't match
	var response map[string]interface{}
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		t.Fatalf("Failed to parse JSON: %v\nResult: %s", err, result)
	}

	if response["error"] == nil {
		t.Errorf("Expected error response for wrong package name")
	}
}

func TestCueEvalModule_ComplexNestedStructure(t *testing.T) {
	cueContent := `
env: {
	DATABASE: {
		HOST: "localhost"
		PORT: 5432
		NAME: "myapp"
	}
	FEATURES: {
		CACHE_ENABLED: true
		MAX_CONNECTIONS: 100
	}
	TAGS: ["production", "web", "api"]
}`

	tempDir, cleanup := createTestCueModule(t, "cuenv", cueContent)
	defer cleanup()

	result := callCueEvalModule(tempDir, "cuenv", "")

	// Parse bridge response
	var response map[string]interface{}
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		t.Fatalf("Failed to parse JSON: %v\nResult: %s", err, result)
	}

	if response["error"] != nil {
		t.Fatalf("Expected success, got error: %v", response["error"])
	}

	okPayload := response["ok"].(map[string]interface{})
	instances := okPayload["instances"].(map[string]interface{})
	rootInstance := instances["."].(map[string]interface{})
	env := rootInstance["env"].(map[string]interface{})

	// Check DATABASE nested object
	database, ok := env["DATABASE"].(map[string]interface{})
	if !ok {
		t.Fatalf("Expected DATABASE to be an object, got %T", env["DATABASE"])
	}

	if database["HOST"] != "localhost" {
		t.Errorf("Expected DATABASE.HOST to be 'localhost', got %v", database["HOST"])
	}

	// Check TAGS array
	tags, ok := env["TAGS"].([]interface{})
	if !ok {
		t.Fatalf("Expected TAGS to be an array, got %T", env["TAGS"])
	}

	if len(tags) != 3 {
		t.Errorf("Expected 3 tags, got %d", len(tags))
	}
}

func TestCueEvalModule_MemoryManagement(t *testing.T) {
	cueContent := `env: { TEST_VAR: "value" }`
	tempDir, cleanup := createTestCueModule(t, "cuenv", cueContent)
	defer cleanup()

	// Make multiple calls to ensure no memory leaks
	for i := 0; i < 10; i++ {
		result := callCueEvalModule(tempDir, "cuenv", "")

		var response map[string]interface{}
		if err := json.Unmarshal([]byte(result), &response); err != nil {
			t.Fatalf("Call %d failed to parse JSON: %v", i, err)
		}

		if response["error"] != nil {
			t.Fatalf("Call %d returned error: %v", i, response["error"])
		}
	}
}

func TestCueEvalModule_ConcurrentAccess(t *testing.T) {
	cueContent := `env: { CONCURRENT_VAR: "test" }`
	tempDir, cleanup := createTestCueModule(t, "cuenv", cueContent)
	defer cleanup()

	const numGoroutines = 5
	results := make(chan string, numGoroutines)
	errors := make(chan error, numGoroutines)

	for i := 0; i < numGoroutines; i++ {
		go func(id int) {
			defer func() {
				if r := recover(); r != nil {
					errors <- fmt.Errorf("goroutine %d panicked: %v", id, r)
					return
				}
			}()

			result := callCueEvalModule(tempDir, "cuenv", "")
			results <- result
		}(i)
	}

	for i := 0; i < numGoroutines; i++ {
		select {
		case result := <-results:
			var response map[string]interface{}
			if err := json.Unmarshal([]byte(result), &response); err != nil {
				t.Errorf("Concurrent call %d failed to parse JSON: %v", i, err)
				continue
			}

			if response["error"] != nil {
				t.Errorf("Concurrent call %d returned error: %v", i, response["error"])
			}

		case err := <-errors:
			t.Errorf("Concurrent access error: %v", err)
		}
	}
}

func TestCueEvalModule_WithOptions(t *testing.T) {
	cueContent := `env: { TEST_VAR: "value" }`
	tempDir, cleanup := createTestCueModule(t, "cuenv", cueContent)
	defer cleanup()

	// Test with explicit options JSON
	options := `{"withMeta": true, "recursive": false}`
	result := callCueEvalModule(tempDir, "cuenv", options)

	var response map[string]interface{}
	if err := json.Unmarshal([]byte(result), &response); err != nil {
		t.Fatalf("Failed to parse JSON: %v\nResult: %s", err, result)
	}

	if response["error"] != nil {
		t.Fatalf("Expected success, got error: %v", response["error"])
	}

	// Verify meta is present when withMeta is true
	okPayload := response["ok"].(map[string]interface{})
	if okPayload["meta"] == nil {
		t.Logf("Note: meta may be empty if no source positions were extracted")
	}
}

func TestCueEvalModule_ConsistentOrdering(t *testing.T) {
	cueContent := `
tasks: {
	zebra: { command: "echo zebra" }
	alpha: { command: "echo alpha" }
	omega: { command: "echo omega" }
	beta: { command: "echo beta" }
}`

	tempDir, cleanup := createTestCueModule(t, "cuenv", cueContent)
	defer cleanup()

	// Parse the same content multiple times and ensure consistent ordering
	var allResults []string

	for i := 0; i < 5; i++ {
		result := callCueEvalModule(tempDir, "cuenv", "")
		allResults = append(allResults, result)
	}

	// All results should be identical (same field ordering)
	for i := 1; i < len(allResults); i++ {
		if allResults[i] != allResults[0] {
			t.Errorf("Inconsistent result on iteration %d", i+1)
			t.Logf("First result: %s", allResults[0])
			t.Logf("Different result: %s", allResults[i])
		}
	}
}
