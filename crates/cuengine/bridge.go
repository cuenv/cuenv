package main

/*
#include <stdlib.h>
*/
import "C"
import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"unsafe"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/load"
)

//export cue_free_string
func cue_free_string(s *C.char) {
	C.free(unsafe.Pointer(s))
}

//export cue_eval_package
func cue_eval_package(dirPath *C.char, packageName *C.char) *C.char {
	// Add recover to catch any panics
	var result *C.char
	defer func() {
		if r := recover(); r != nil {
			errMsg := map[string]string{"error": fmt.Sprintf("Internal error: %v", r)}
			errBytes, _ := json.Marshal(errMsg)
			result = C.CString(string(errBytes))
		}
	}()

	goDir := C.GoString(dirPath)
	goPackageName := C.GoString(packageName)

	// Validate inputs
	if goDir == "" {
		errMsg := map[string]string{"error": "Directory path cannot be empty"}
		errBytes, _ := json.Marshal(errMsg)
		result = C.CString(string(errBytes))
		return result
	}

	if goPackageName == "" {
		errMsg := map[string]string{"error": "Package name cannot be empty"}
		errBytes, _ := json.Marshal(errMsg)
		result = C.CString(string(errBytes))
		return result
	}

	// Create CUE context
	ctx := cuecontext.New()

	// Create load configuration to load from specific directory
	cfg := &load.Config{
		Dir: goDir,
	}

	// Load the specific CUE package by name
	// This matches the behavior of "cue export .:package-name" but from a specific directory
	var instances []*build.Instance
	packagePath := ".:" + goPackageName
	instances = load.Instances([]string{packagePath}, cfg)

	if len(instances) == 0 {
		errMsg := map[string]string{"error": "No CUE instances found"}
		errBytes, _ := json.Marshal(errMsg)
		result = C.CString(string(errBytes))
		return result
	}

	inst := instances[0]
	if inst.Err != nil {
		errMsg := map[string]string{"error": fmt.Sprintf("Failed to load CUE instance: %v", inst.Err)}
		errBytes, _ := json.Marshal(errMsg)
		result = C.CString(string(errBytes))
		return result
	}

	// Build the CUE value
	v := ctx.BuildInstance(inst)
	if v.Err() != nil {
		errMsg := map[string]string{"error": fmt.Sprintf("Failed to build CUE value: %v", v.Err())}
		errBytes, _ := json.Marshal(errMsg)
		result = C.CString(string(errBytes))
		return result
	}

	// Build JSON manually by iterating through CUE fields in order
	// This completely bypasses Go's map randomization
	jsonStr, err := buildOrderedJSONString(v)
	if err != nil {
		errMsg := map[string]string{"error": fmt.Sprintf("Failed to build ordered JSON: %v", err)}
		errBytes, _ := json.Marshal(errMsg)
		result = C.CString(string(errBytes))
		return result
	}
	
	jsonBytes := []byte(jsonStr)

	result = C.CString(string(jsonBytes))
	return result
}

// buildOrderedJSONString manually builds a JSON string from CUE value preserving field order
func buildOrderedJSONString(v cue.Value) (string, error) {
	switch v.Kind() {
	case cue.StructKind:
		var parts []string
		
		// Iterate through fields in the order they appear in CUE
		fields, err := v.Fields(cue.Optional(true))
		if err != nil {
			return "", fmt.Errorf("failed to get fields: %v", err)
		}
		
		for fields.Next() {
			fieldName := fields.Label()
			fieldValue := fields.Value()
			
			// Build JSON key
			keyJSON, err := json.Marshal(fieldName)
			if err != nil {
				return "", fmt.Errorf("failed to marshal field name %s: %v", fieldName, err)
			}
			
			// Recursively build value JSON
			valueJSON, err := buildOrderedJSONString(fieldValue)
			if err != nil {
				return "", fmt.Errorf("failed to build JSON for field %s: %v", fieldName, err)
			}
			
			// Combine key:value
			parts = append(parts, string(keyJSON)+":"+valueJSON)
		}
		
		return "{" + strings.Join(parts, ",") + "}", nil
		
	case cue.ListKind:
		var parts []string
		
		// Iterate through list items
		list, err := v.List()
		if err != nil {
			return "", fmt.Errorf("failed to get list: %v", err)
		}
		
		for list.Next() {
			itemJSON, err := buildOrderedJSONString(list.Value())
			if err != nil {
				return "", err
			}
			parts = append(parts, itemJSON)
		}
		
		return "[" + strings.Join(parts, ",") + "]", nil
		
	default:
		// For primitive types, use standard JSON marshaling
		var val interface{}
		if err := v.Decode(&val); err != nil {
			return "", fmt.Errorf("failed to decode primitive value: %v", err)
		}
		
		jsonBytes, err := json.Marshal(val)
		if err != nil {
			return "", fmt.Errorf("failed to marshal primitive value: %v", err)
		}
		
		return string(jsonBytes), nil
	}
}

func main() {}