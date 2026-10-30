package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"fmt"
	"net/http"
	"path/filepath"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/load"
	"cuelang.org/go/mod/modconfig"
)

//export cue_eval_file
func cue_eval_file(filePath *C.char) *C.char {
	return C.CString(evaluateFileResponse(C.GoString(filePath)))
}

// evaluateFileResponse loads one explicitly named file in its original module.
// Unlike a directory query this does not unify neighbouring package files.
func evaluateFileResponse(filePath string) (result string) {
	defer func() {
		if recovered := recover(); recovered != nil {
			result = createErrorResponse(ErrorCodePanicRecover, fmt.Sprintf("Internal panic: %v", recovered), nil)
		}
	}()
	if filePath == "" {
		return createErrorResponse(ErrorCodeInvalidInput, "CUE file path cannot be empty", nil)
	}
	file, err := filepath.Abs(filePath)
	if err != nil {
		return createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Cannot resolve CUE file path: %v", err), nil)
	}
	directory := filepath.Dir(file)
	registry, err := modconfig.NewRegistry(&modconfig.Config{Transport: http.DefaultTransport, ClientType: "cuenv"})
	if err != nil {
		return createErrorResponse(ErrorCodeRegistryInit, fmt.Sprintf("Failed to initialize CUE registry: %v", err), nil)
	}
	instances := load.Instances([]string{file}, &load.Config{Dir: directory, Registry: registry})
	if len(instances) != 1 {
		return createErrorResponse(ErrorCodeLoadInstance, fmt.Sprintf("Expected one CUE instance for %s, got %d", file, len(instances)), nil)
	}
	if err := instances[0].Err; err != nil {
		return createErrorResponse(ErrorCodeLoadInstance, errorDetails(err, directory), nil)
	}
	value := cuecontext.New().BuildInstance(instances[0])
	if err := value.Validate(cue.Concrete(true), cue.Final()); err != nil {
		return createErrorResponse(ErrorCodeBuildValue, errorDetails(err, directory), nil)
	}
	encoded, err := value.MarshalJSON()
	if err != nil {
		return createErrorResponse(ErrorCodeJSONMarshal, errorDetails(err, directory), nil)
	}
	return createSuccessResponse(string(encoded))
}
