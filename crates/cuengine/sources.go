package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"net/http"
	"path/filepath"
	"sort"
	"strings"

	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/load"
	"cuelang.org/go/mod/modconfig"
)

// instanceSources is the payload of cue_instance_sources.
type instanceSources struct {
	// Files the instance is built from, relative to the module root and
	// sorted: its own files (including the same-package files of ancestor
	// directories that CUE unifies into it) and the files of every package
	// it imports, transitively, that lives inside the module.
	Files []string `json:"files"`
}

//export cue_instance_sources
func cue_instance_sources(moduleRootPath *C.char, directoryPath *C.char, packageName *C.char) *C.char {
	return C.CString(instanceSourcesResponse(C.GoString(moduleRootPath), C.GoString(directoryPath), C.GoString(packageName)))
}

// instanceSourcesResponse implements cue_instance_sources on Go strings so it
// can be exercised without cgo. It loads the instance without building or
// evaluating it.
func instanceSourcesResponse(moduleRoot string, directory string, packageName string) (result string) {
	defer func() {
		if r := recover(); r != nil {
			result = createErrorResponse(ErrorCodePanicRecover, fmt.Sprintf("Internal panic: %v", r), nil)
		}
	}()

	if moduleRoot == "" || directory == "" || packageName == "" {
		return createErrorResponse(ErrorCodeInvalidInput, "Module root, directory and package name cannot be empty", nil)
	}
	absoluteModuleRoot, err := filepath.Abs(moduleRoot)
	if err != nil {
		return createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Cannot resolve CUE root path: %v", err), nil)
	}
	absoluteDirectory, err := filepath.Abs(directory)
	if err != nil {
		return createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Cannot resolve CUE directory: %v", err), nil)
	}

	registry, err := modconfig.NewRegistry(&modconfig.Config{
		Transport:  http.DefaultTransport,
		ClientType: "cuenv",
	})
	if err != nil {
		hint := "Check CUE registry configuration (CUE_REGISTRY env var) and network access"
		return createErrorResponse(ErrorCodeRegistryInit, fmt.Sprintf("Failed to initialize CUE registry: %v", err), &hint)
	}

	instances := load.Instances([]string{"."}, &load.Config{
		Dir:        absoluteDirectory,
		ModuleRoot: absoluteModuleRoot,
		Registry:   registry,
		Package:    packageName,
	})
	if len(instances) == 0 {
		return createErrorResponse(ErrorCodeLoadInstance, fmt.Sprintf("No CUE package '%s' in %s", packageName, absoluteDirectory), nil)
	}
	if instances[0].Err != nil {
		return createErrorResponse(ErrorCodeLoadInstance, instances[0].Err.Error(), nil)
	}

	files := map[string]struct{}{}
	collectInstanceSources(instances[0], absoluteModuleRoot, files, map[*build.Instance]struct{}{})

	payload := instanceSources{Files: make([]string, 0, len(files))}
	for file := range files {
		payload.Files = append(payload.Files, file)
	}
	sort.Strings(payload.Files)

	encoded, err := json.Marshal(payload)
	if err != nil {
		return createErrorResponse(ErrorCodeJSONMarshal, fmt.Sprintf("Failed to marshal instance sources: %v", err), nil)
	}
	return createSuccessResponse(string(encoded))
}

// collectInstanceSources adds the module-relative files of inst and of every
// instance it imports. Files outside the module (registry dependencies in the
// module cache) are left out.
func collectInstanceSources(inst *build.Instance, moduleRoot string, files map[string]struct{}, seen map[*build.Instance]struct{}) {
	if _, done := seen[inst]; done {
		return
	}
	seen[inst] = struct{}{}

	for _, file := range inst.BuildFiles {
		relative, err := filepath.Rel(moduleRoot, file.Filename)
		if err != nil || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) || filepath.IsAbs(relative) {
			continue
		}
		files[filepath.ToSlash(relative)] = struct{}{}
	}
	for _, imported := range inst.Imports {
		collectInstanceSources(imported, moduleRoot, files, seen)
	}
}
