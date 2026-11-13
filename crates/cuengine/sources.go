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
	"strconv"
	"strings"

	"cuelang.org/go/cue/ast"
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
	// What the @embed attributes of those files read, relative to the module
	// root and sorted: a path for `file=`, a glob for `glob=`. They are read
	// from the source, so a file a glob would match is covered whether or not
	// it exists.
	Embeds []string `json:"embeds"`
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

	collected := sourceSet{files: map[string]struct{}{}, embeds: map[string]struct{}{}}
	collectInstanceSources(instances[0], absoluteModuleRoot, collected, map[*build.Instance]struct{}{})

	payload := instanceSources{Files: sortedKeys(collected.files), Embeds: sortedKeys(collected.embeds)}

	encoded, err := json.Marshal(payload)
	if err != nil {
		return createErrorResponse(ErrorCodeJSONMarshal, fmt.Sprintf("Failed to marshal instance sources: %v", err), nil)
	}
	return createSuccessResponse(string(encoded))
}

// sourceSet is what an instance and its imports are built from.
type sourceSet struct {
	files  map[string]struct{}
	embeds map[string]struct{}
}

func sortedKeys(set map[string]struct{}) []string {
	keys := make([]string, 0, len(set))
	for key := range set {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

// moduleRelative is path relative to the module root with forward slashes, or
// false when it lies outside the module.
func moduleRelative(moduleRoot string, path string) (string, bool) {
	relative, err := filepath.Rel(moduleRoot, path)
	if err != nil || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) || filepath.IsAbs(relative) {
		return "", false
	}
	return filepath.ToSlash(relative), true
}

// collectInstanceSources adds the module-relative files of inst and of every
// instance it imports, and what their @embed attributes read. Files outside
// the module (registry dependencies in the module cache) are left out.
func collectInstanceSources(inst *build.Instance, moduleRoot string, collected sourceSet, seen map[*build.Instance]struct{}) {
	if _, done := seen[inst]; done {
		return
	}
	seen[inst] = struct{}{}

	for _, file := range inst.BuildFiles {
		if relative, inside := moduleRelative(moduleRoot, file.Filename); inside {
			collected.files[relative] = struct{}{}
		}
	}
	for _, file := range inst.Files {
		if _, inside := moduleRelative(moduleRoot, file.Filename); !inside {
			continue
		}
		directory := filepath.Dir(file.Filename)
		for _, pattern := range embedPatterns(file) {
			// An @embed path is relative to the directory of its file.
			if relative, inside := moduleRelative(moduleRoot, filepath.Join(directory, filepath.FromSlash(pattern))); inside {
				collected.embeds[relative] = struct{}{}
			}
		}
	}
	for _, imported := range inst.Imports {
		collectInstanceSources(imported, moduleRoot, collected, seen)
	}
}

// embedPatterns lists the `file=` paths and `glob=` patterns of the @embed
// attributes in file, as written.
func embedPatterns(file *ast.File) []string {
	var patterns []string
	ast.Walk(file, func(node ast.Node) bool {
		attribute, isAttribute := node.(*ast.Attribute)
		if !isAttribute {
			return true
		}
		key, body := attribute.Split()
		if key != "embed" {
			return true
		}
		for _, argument := range splitAttributeArguments(body) {
			name, value, hasValue := strings.Cut(argument, "=")
			name = strings.TrimSpace(name)
			if !hasValue || (name != "file" && name != "glob") {
				continue
			}
			if pattern := unquoteAttributeValue(strings.TrimSpace(value)); pattern != "" {
				patterns = append(patterns, pattern)
			}
		}
		return true
	}, nil)
	return patterns
}

// splitAttributeArguments splits the body of an attribute at the commas that
// are not inside a quoted string.
func splitAttributeArguments(body string) []string {
	var arguments []string
	start := 0
	quote := byte(0)
	for index := 0; index < len(body); index++ {
		switch char := body[index]; {
		case quote != 0:
			if char == '\\' {
				index++
			} else if char == quote {
				quote = 0
			}
		case char == '"' || char == '\'':
			quote = char
		case char == ',':
			arguments = append(arguments, body[start:index])
			start = index + 1
		}
	}
	return append(arguments, body[start:])
}

// unquoteAttributeValue is the value of an attribute argument, which may be
// written bare or as a quoted string.
func unquoteAttributeValue(value string) string {
	if len(value) >= 2 && value[0] == '"' {
		if unquoted, err := strconv.Unquote(value); err == nil {
			return unquoted
		}
	}
	return strings.Trim(value, `"'`)
}
