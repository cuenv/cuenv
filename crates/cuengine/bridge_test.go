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

// Test data structure for validation
type TestCueData struct {
	Env map[string]interface{} `json:"env"`
}

// Helper function to create a temporary directory with CUE files
func createTestCueDir(t *testing.T, packageName string, content string) (string, func()) {
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

// Helper to call FFI function safely
func callCueEvalPackage(dirPath, packageName string) string {
	cDirPath := C.CString(dirPath)
	cPackageName := C.CString(packageName)
	defer C.free(unsafe.Pointer(cDirPath))
	defer C.free(unsafe.Pointer(cPackageName))

	result := cue_eval_package(cDirPath, cPackageName)
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

func TestCueEvalPackage_ValidInput(t *testing.T) {
	cueContent := `
env: {
	DATABASE_URL: "postgres://localhost/mydb"
	API_KEY: "test-key"
	PORT: 3000
	DEBUG: true
}`

	tempDir, cleanup := createTestCueDir(t, "cuenv", cueContent)
	defer cleanup()

	result := callCueEvalPackage(tempDir, "cuenv")

	// Parse result
	var data TestCueData
	if err := json.Unmarshal([]byte(result), &data); err != nil {
		t.Fatalf("Failed to parse JSON result: %v\nResult: %s", err, result)
	}

	// Verify expected values
	if data.Env["DATABASE_URL"] != "postgres://localhost/mydb" {
		t.Errorf("Expected DATABASE_URL to be 'postgres://localhost/mydb', got %v", data.Env["DATABASE_URL"])
	}

	if data.Env["API_KEY"] != "test-key" {
		t.Errorf("Expected API_KEY to be 'test-key', got %v", data.Env["API_KEY"])
	}

	// PORT should be parsed as number
	if port, ok := data.Env["PORT"].(float64); !ok || port != 3000 {
		t.Errorf("Expected PORT to be 3000 (number), got %v (%T)", data.Env["PORT"], data.Env["PORT"])
	}

	// DEBUG should be parsed as boolean
	if debug, ok := data.Env["DEBUG"].(bool); !ok || debug != true {
		t.Errorf("Expected DEBUG to be true (boolean), got %v (%T)", data.Env["DEBUG"], data.Env["DEBUG"])
	}
}

func TestCueEvalPackage_EmptyDirectory(t *testing.T) {
	result := callCueEvalPackage("", "cuenv")

	// Should return error JSON
	var errorResponse map[string]string
	if err := json.Unmarshal([]byte(result), &errorResponse); err != nil {
		t.Fatalf("Failed to parse error JSON: %v\nResult: %s", err, result)
	}

	if errorResponse["error"] != "Directory path cannot be empty" {
		t.Errorf("Expected specific error message, got: %s", errorResponse["error"])
	}
}

func TestCueEvalPackage_EmptyPackageName(t *testing.T) {
	tempDir, cleanup := createTestCueDir(t, "cuenv", "env: {}")
	defer cleanup()

	result := callCueEvalPackage(tempDir, "")

	// Should return error JSON
	var errorResponse map[string]string
	if err := json.Unmarshal([]byte(result), &errorResponse); err != nil {
		t.Fatalf("Failed to parse error JSON: %v\nResult: %s", err, result)
	}

	if errorResponse["error"] != "Package name cannot be empty" {
		t.Errorf("Expected specific error message, got: %s", errorResponse["error"])
	}
}

func TestCueEvalPackage_NonexistentDirectory(t *testing.T) {
	result := callCueEvalPackage("/nonexistent/path", "cuenv")

	// Should return error JSON
	var errorResponse map[string]string
	if err := json.Unmarshal([]byte(result), &errorResponse); err != nil {
		t.Fatalf("Failed to parse error JSON: %v\nResult: %s", err, result)
	}

	if !strings.Contains(errorResponse["error"], "Failed to change directory") {
		t.Errorf("Expected directory change error, got: %s", errorResponse["error"])
	}
}

func TestCueEvalPackage_InvalidCueSyntax(t *testing.T) {
	invalidCueContent := `
env: {
	INVALID_SYNTAX: "missing closing brace"
`
	tempDir, cleanup := createTestCueDir(t, "cuenv", invalidCueContent)
	defer cleanup()

	result := callCueEvalPackage(tempDir, "cuenv")

	// Should return error JSON
	var errorResponse map[string]string
	if err := json.Unmarshal([]byte(result), &errorResponse); err != nil {
		t.Fatalf("Failed to parse error JSON: %v\nResult: %s", err, result)
	}

	// Should contain some indication of CUE error
	if !strings.Contains(errorResponse["error"], "Failed to") {
		t.Errorf("Expected CUE parsing error, got: %s", errorResponse["error"])
	}
}

func TestCueEvalPackage_WrongPackageName(t *testing.T) {
	cueContent := `env: { TEST_VAR: "value" }`
	tempDir, cleanup := createTestCueDir(t, "wrongpackage", cueContent)
	defer cleanup()

	result := callCueEvalPackage(tempDir, "cuenv")

	// Should return error JSON since package name doesn't match
	var errorResponse map[string]string
	if err := json.Unmarshal([]byte(result), &errorResponse); err != nil {
		t.Fatalf("Failed to parse error JSON: %v\nResult: %s", err, result)
	}

	// Should indicate that no instances were found or there was a loading error
	errorMsg := errorResponse["error"]
	if !strings.Contains(errorMsg, "No CUE instances found") && !strings.Contains(errorMsg, "Failed to load CUE instance") {
		t.Errorf("Expected package loading error, got: %s", errorMsg)
	}
}

func TestCueEvalPackage_ComplexNestedStructure(t *testing.T) {
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

	tempDir, cleanup := createTestCueDir(t, "cuenv", cueContent)
	defer cleanup()

	result := callCueEvalPackage(tempDir, "cuenv")

	// Parse result
	var data map[string]interface{}
	if err := json.Unmarshal([]byte(result), &data); err != nil {
		t.Fatalf("Failed to parse JSON result: %v\nResult: %s", err, result)
	}

	// Verify nested structure exists
	env, ok := data["env"].(map[string]interface{})
	if !ok {
		t.Fatalf("Expected env to be an object, got %T", data["env"])
	}

	// Check DATABASE nested object
	database, ok := env["DATABASE"].(map[string]interface{})
	if !ok {
		t.Fatalf("Expected DATABASE to be an object, got %T", env["DATABASE"])
	}

	if database["HOST"] != "localhost" {
		t.Errorf("Expected DATABASE.HOST to be 'localhost', got %v", database["HOST"])
	}

	if port, ok := database["PORT"].(float64); !ok || port != 5432 {
		t.Errorf("Expected DATABASE.PORT to be 5432, got %v (%T)", database["PORT"], database["PORT"])
	}

	// Check TAGS array
	tags, ok := env["TAGS"].([]interface{})
	if !ok {
		t.Fatalf("Expected TAGS to be an array, got %T", env["TAGS"])
	}

	if len(tags) != 3 {
		t.Errorf("Expected 3 tags, got %d", len(tags))
	}

	if tags[0] != "production" {
		t.Errorf("Expected first tag to be 'production', got %v", tags[0])
	}
}

func TestCueEvalPackage_MemoryManagement(t *testing.T) {
	// Test that multiple calls don't leak memory or cause crashes
	cueContent := `env: { TEST_VAR: "value" }`
	tempDir, cleanup := createTestCueDir(t, "cuenv", cueContent)
	defer cleanup()

	// Make multiple calls to ensure no memory leaks
	for i := 0; i < 10; i++ {
		result := callCueEvalPackage(tempDir, "cuenv")

		// Basic validation that it returns valid JSON
		var data map[string]interface{}
		if err := json.Unmarshal([]byte(result), &data); err != nil {
			t.Fatalf("Call %d failed to parse JSON: %v", i, err)
		}

		// Verify expected structure
		if env, ok := data["env"].(map[string]interface{}); !ok {
			t.Fatalf("Call %d: expected env object", i)
		} else if env["TEST_VAR"] != "value" {
			t.Errorf("Call %d: expected TEST_VAR='value', got %v", i, env["TEST_VAR"])
		}
	}
}

func TestCueEvalPackage_ConcurrentAccess(t *testing.T) {
	// Test concurrent calls to ensure thread safety
	cueContent := `env: { CONCURRENT_VAR: "test" }`
	tempDir, cleanup := createTestCueDir(t, "cuenv", cueContent)
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

			result := callCueEvalPackage(tempDir, "cuenv")
			results <- result
		}(i)
	}

	// Collect results
	for i := 0; i < numGoroutines; i++ {
		select {
		case result := <-results:
			var data map[string]interface{}
			if err := json.Unmarshal([]byte(result), &data); err != nil {
				t.Errorf("Concurrent call %d failed to parse JSON: %v", i, err)
				continue
			}

			env, ok := data["env"].(map[string]interface{})
			if !ok {
				t.Errorf("Concurrent call %d: expected env object", i)
				continue
			}

			if env["CONCURRENT_VAR"] != "test" {
				t.Errorf("Concurrent call %d: expected CONCURRENT_VAR='test', got %v", i, env["CONCURRENT_VAR"])
			}

		case err := <-errors:
			t.Errorf("Concurrent access error: %v", err)
		}
	}
}

func TestFieldOrderingPreservation(t *testing.T) {
	// Create a CUE file with tasks in a specific order
	cueContent := `
tasks: {
	ordered_group: {
		description: "Test field ordering"
		mode: "sequential"
		
		// These should appear in this exact order in JSON
		first: {
			command: "echo first"
		}
		second: {
			command: "echo second"
		}
		third: {
			command: "echo third"
		}
		fourth: {
			command: "echo fourth"
		}
	}
}`

	tempDir, cleanup := createTestCueDir(t, "cuenv", cueContent)
	defer cleanup()

	result := callCueEvalPackage(tempDir, "cuenv")

	// Parse the JSON to check for errors
	var data map[string]interface{}
	if err := json.Unmarshal([]byte(result), &data); err != nil {
		if strings.Contains(result, "error") {
			t.Fatalf("CUE evaluation failed: %s", result)
		}
		t.Fatalf("Failed to parse JSON result: %v", err)
	}

	// Navigate to tasks.ordered_group
	tasks, ok := data["tasks"].(map[string]interface{})
	if !ok {
		t.Fatalf("Expected tasks to be a map, got: %T", data["tasks"])
	}

	orderedGroup, ok := tasks["ordered_group"].(map[string]interface{})
	if !ok {
		t.Fatalf("Expected ordered_group to be a map, got: %T", tasks["ordered_group"])
	}

	// The critical test: check if the JSON string contains the fields in the right order
	// Since Go maps are unordered, we need to check the JSON string directly
	expectedOrder := []string{"first", "second", "third", "fourth"}

	// Find the positions of each field name in the JSON string
	positions := make(map[string]int)
	for _, field := range expectedOrder {
		// Look for the field as a JSON key (with quotes and colon)
		searchStr := `"` + field + `":`
		pos := strings.Index(result, searchStr)
		if pos == -1 {
			t.Fatalf("Field %s not found in JSON", field)
		}
		positions[field] = pos
	}

	// Verify the positions are in ascending order (matching CUE definition order)
	for i := 1; i < len(expectedOrder); i++ {
		prevField := expectedOrder[i-1]
		currField := expectedOrder[i]

		if positions[prevField] >= positions[currField] {
			t.Errorf("Field ordering incorrect: %s (pos %d) should come before %s (pos %d)",
				prevField, positions[prevField], currField, positions[currField])
			t.Logf("Full JSON result: %s", result)
		}
	}

	// Also verify all expected fields are present in the ordered_group
	for _, field := range expectedOrder {
		if _, exists := orderedGroup[field]; !exists {
			t.Errorf("Expected field %s not found in ordered_group", field)
		}
	}

	if t.Failed() {
		t.Logf("Field ordering test failed. JSON positions: %v", positions)
	} else {
		t.Logf("✓ Field ordering test passed. JSON positions: %v", positions)
	}
}

func TestConsistentOrdering(t *testing.T) {
	cueContent := `
tasks: {
	consistency_test: {
		mode: "sequential"
		zebra: { command: "echo zebra" }
		alpha: { command: "echo alpha" }
		omega: { command: "echo omega" }
		beta: { command: "echo beta" }
	}
}`

	tempDir, cleanup := createTestCueDir(t, "cuenv", cueContent)
	defer cleanup()

	// Parse the same content multiple times and ensure consistent ordering
	var allResults []string

	for i := 0; i < 5; i++ {
		result := callCueEvalPackage(tempDir, "cuenv")
		allResults = append(allResults, result)
	}

	// All results should be identical (same field ordering)
	for i := 1; i < len(allResults); i++ {
		if allResults[i] != allResults[0] {
			t.Errorf("Inconsistent result on iteration %d", i+1)
			t.Logf("First result: %s", allResults[0])
			t.Logf("Different result: %s", allResults[i])

			// Show where they differ
			if len(allResults[0]) == len(allResults[i]) {
				for j := 0; j < len(allResults[0]); j++ {
					if allResults[0][j] != allResults[i][j] {
						t.Logf("First difference at position %d: %c vs %c", j, allResults[0][j], allResults[i][j])
						break
					}
				}
			}
		}
	}

	if !t.Failed() {
		t.Logf("✓ Consistency test passed across %d iterations", len(allResults))
	}
}
