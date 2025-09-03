package main

/*
#include <stdlib.h>
*/
import "C"
import (
	"encoding/json"
	"fmt"
	"runtime"
	"strings"
	"unsafe"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/load"
)

const BridgeVersion = "bridge/1"

// Bridge error codes - keep in sync with Rust side
const (
	ErrorCodeInvalidInput   = "INVALID_INPUT"
	ErrorCodeLoadInstance   = "LOAD_INSTANCE"
	ErrorCodeBuildValue     = "BUILD_VALUE"
	ErrorCodeOrderedJSON    = "ORDERED_JSON"
	ErrorCodePanicRecover   = "PANIC_RECOVER"
	ErrorCodeJSONMarshal    = "JSON_MARSHAL_ERROR"
)

// BridgeError represents an error in the bridge response
type BridgeError struct {
	Code    string  `json:"code"`
	Message string  `json:"message"`
	Hint    *string `json:"hint,omitempty"`
}

// BridgeResponse represents the structured response envelope
type BridgeResponse struct {
	Version string       `json:"version"`
	Ok      *json.RawMessage `json:"ok,omitempty"`
	Error   *BridgeError `json:"error,omitempty"`
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

	// Build JSON manually by iterating through CUE fields in order
	// This completely bypasses Go's map randomization
	jsonStr, err := buildOrderedJSONString(v)
	if err != nil {
		msg := fmt.Sprintf("Failed to build ordered JSON: %v", err)
		result = createErrorResponse(ErrorCodeOrderedJSON, msg, nil)
		return result
	}

	// Return success response with ordered JSON data
	result = createSuccessResponse(jsonStr)
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
