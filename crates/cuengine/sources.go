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

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/literal"
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
	// The module path of the instance.
	Module string `json:"module"`
	// One record per package the instance imports, transitively, sorted by
	// import path. Listed only when the caller asks for fields.
	Imports []importedInstance `json:"imports"`
}

// instanceSourcesOptions is what a caller of cue_instance_sources asks for
// beyond the files. An empty options string asks for nothing more.
type instanceSourcesOptions struct {
	// CUE paths whose values are read from every imported instance, and
	// from the instances of the same package in the directories above it.
	Fields []string `json:"fields"`
}

// importedInstance is one package an instance imports.
type importedInstance struct {
	// The import path, as written by the package that imports it.
	ImportPath string `json:"importPath"`
	// The import path of the package that imports it first; empty when the
	// instance itself does.
	ImportedBy string `json:"importedBy"`
	// The directory of the package: relative to the module root when it lies
	// inside it, absolute otherwise.
	Directory string `json:"directory"`
	// The package name.
	Package string `json:"package"`
	// The path of the module the package belongs to.
	Module string `json:"module"`
	// The values of the requested fields that are concrete, by field path.
	// A field that is absent, incomplete or in error is left out.
	Fields map[string]json.RawMessage `json:"fields"`
	// The same fields read from the instance of the same package in each
	// directory above the package that holds files of it, nearest first.
	Ancestors []ancestorInstance `json:"ancestors"`
}

// ancestorInstance is the instance of an imported package's own package in a
// directory above it.
type ancestorInstance struct {
	// The directory, relative to the module root.
	Directory string `json:"directory"`
	// The values of the requested fields that are concrete.
	Fields map[string]json.RawMessage `json:"fields"`
}

//export cue_instance_sources
func cue_instance_sources(moduleRootPath *C.char, directoryPath *C.char, packageName *C.char, optionsJSON *C.char) *C.char {
	return C.CString(instanceSourcesResponseWith(sourcesRequest{
		moduleRoot:  C.GoString(moduleRootPath),
		directory:   C.GoString(directoryPath),
		packageName: C.GoString(packageName),
		options:     C.GoString(optionsJSON),
	}))
}

// sourcesRequest is one call of cue_instance_sources on Go strings.
type sourcesRequest struct {
	moduleRoot  string
	directory   string
	packageName string
	// JSON of instanceSourcesOptions; empty for none.
	options string
}

// instanceSourcesResponse lists the files an instance is built from, asking
// for no fields.
func instanceSourcesResponse(moduleRoot string, directory string, packageName string) string {
	return instanceSourcesResponseWith(sourcesRequest{moduleRoot: moduleRoot, directory: directory, packageName: packageName})
}

// instanceSourcesResponseWith implements cue_instance_sources on Go strings
// so it can be exercised without cgo. It loads the instance without building
// or evaluating it. Only when fields are requested is each imported package
// built, on its own, to look them up; the instance itself is never built,
// and a package whose root is in error still yields the fields that are not.
func instanceSourcesResponseWith(request sourcesRequest) (result string) {
	defer func() {
		if r := recover(); r != nil {
			result = createErrorResponse(ErrorCodePanicRecover, fmt.Sprintf("Internal panic: %v", r), nil)
		}
	}()

	moduleRoot, directory, packageName := request.moduleRoot, request.directory, request.packageName
	if moduleRoot == "" || directory == "" || packageName == "" {
		return createErrorResponse(ErrorCodeInvalidInput, "Module root, directory and package name cannot be empty", nil)
	}
	var options instanceSourcesOptions
	if request.options != "" {
		if err := json.Unmarshal([]byte(request.options), &options); err != nil {
			return createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Invalid instance sources options: %v", err), nil)
		}
	}
	fields := make([]cue.Path, 0, len(options.Fields))
	for _, field := range options.Fields {
		path := cue.ParsePath(field)
		if path.Err() != nil {
			return createErrorResponse(ErrorCodeInvalidInput, fmt.Sprintf("Invalid field path %q: %v", field, path.Err()), nil)
		}
		fields = append(fields, path)
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

	payload := instanceSources{
		Files:   sortedKeys(collected.files),
		Embeds:  sortedKeys(collected.embeds),
		Module:  instances[0].Module,
		Imports: []importedInstance{},
	}
	if len(fields) > 0 {
		reader := fieldReader{
			moduleRoot: absoluteModuleRoot,
			registry:   registry,
			fields:     options.Fields,
			paths:      fields,
			ancestors:  map[string]map[string]json.RawMessage{},
		}
		payload.Imports = reader.importedInstances(instances[0])
	}

	encoded, err := json.Marshal(payload)
	if err != nil {
		return createErrorResponse(ErrorCodeJSONMarshal, fmt.Sprintf("Failed to marshal instance sources: %v", err), nil)
	}
	return createSuccessResponse(string(encoded))
}

// fieldReader reads requested fields from the packages an instance imports.
type fieldReader struct {
	moduleRoot string
	registry   modconfig.Registry
	// The requested field paths, as written and parsed.
	fields []string
	paths  []cue.Path
	// The fields of the instance of a package in a directory, by directory
	// and package, so each is loaded and built once per call.
	ancestors map[string]map[string]json.RawMessage
	// The context packages are built in. Built packages are cached by it,
	// so a package imported by several others is built once.
	ctx *cue.Context
}

// importedInstances lists one record per package root imports, transitively,
// sorted by import path. A package imported by several is listed once, as
// imported by the first one met, nearest the instance first.
func (r *fieldReader) importedInstances(root *build.Instance) []importedInstance {
	r.ctx = cuecontext.New()
	type pending struct {
		inst     *build.Instance
		importer string
	}
	seen := map[string]struct{}{}
	queue := []pending{}
	for _, imported := range root.Imports {
		queue = append(queue, pending{inst: imported})
	}
	var records []importedInstance
	for len(queue) > 0 {
		next := queue[0]
		queue = queue[1:]
		if _, done := seen[next.inst.ImportPath]; done {
			continue
		}
		seen[next.inst.ImportPath] = struct{}{}
		records = append(records, r.record(next.inst, next.importer))
		for _, imported := range next.inst.Imports {
			queue = append(queue, pending{inst: imported, importer: next.inst.ImportPath})
		}
	}
	sort.Slice(records, func(left, right int) bool {
		return records[left].ImportPath < records[right].ImportPath
	})
	return records
}

// record describes one imported package. The package is built on its own to
// look its fields up; a root in error still yields the fields that are not.
func (r *fieldReader) record(inst *build.Instance, importer string) importedInstance {
	directory := inst.Dir
	if relative, inside := moduleRelative(r.moduleRoot, inst.Dir); inside {
		directory = relative
	}
	return importedInstance{
		ImportPath: inst.ImportPath,
		ImportedBy: importer,
		Directory:  directory,
		Package:    inst.PkgName,
		Module:     inst.Module,
		Fields:     r.read(r.ctx.BuildInstance(inst)),
		Ancestors:  r.ancestorInstances(inst),
	}
}

// ancestorInstances reads the fields of the instance of inst's package in
// each directory above inst that holds files of that package, nearest first.
// A directory in between that holds another package, or no file of this one,
// is not an ancestor and is skipped.
func (r *fieldReader) ancestorInstances(inst *build.Instance) []ancestorInstance {
	directories := map[string]struct{}{}
	for _, file := range inst.BuildFiles {
		directory := filepath.Dir(file.Filename)
		if directory == inst.Dir {
			continue
		}
		if _, inside := moduleRelative(r.moduleRoot, directory); !inside {
			continue
		}
		if relative, err := filepath.Rel(directory, inst.Dir); err != nil || strings.HasPrefix(relative, "..") {
			continue
		}
		directories[directory] = struct{}{}
	}
	ordered := make([]string, 0, len(directories))
	for directory := range directories {
		ordered = append(ordered, directory)
	}
	// Nearest first: a deeper directory has a longer path.
	sort.Slice(ordered, func(left, right int) bool {
		return len(ordered[left]) > len(ordered[right])
	})
	ancestors := make([]ancestorInstance, 0, len(ordered))
	for _, directory := range ordered {
		relative, _ := moduleRelative(r.moduleRoot, directory)
		ancestors = append(ancestors, ancestorInstance{
			Directory: relative,
			Fields:    r.directoryFields(directory, inst.PkgName),
		})
	}
	return ancestors
}

// directoryFields reads the fields of the instance of packageName in
// directory, once per call. One that cannot be loaded has none.
func (r *fieldReader) directoryFields(directory string, packageName string) map[string]json.RawMessage {
	key := directory + "\x00" + packageName
	if known, done := r.ancestors[key]; done {
		return known
	}
	fields := map[string]json.RawMessage{}
	instances := load.Instances([]string{"."}, &load.Config{
		Dir:        directory,
		ModuleRoot: r.moduleRoot,
		Registry:   r.registry,
		Package:    packageName,
	})
	if len(instances) > 0 && instances[0].Err == nil {
		fields = r.read(r.ctx.BuildInstance(instances[0]))
	}
	r.ancestors[key] = fields
	return fields
}

// read looks the requested fields up on value. A field that is absent,
// incomplete or in error is left out; a default is resolved.
func (r *fieldReader) read(value cue.Value) map[string]json.RawMessage {
	fields := map[string]json.RawMessage{}
	for index, path := range r.paths {
		field := value.LookupPath(path)
		if !field.Exists() {
			continue
		}
		resolved, _ := field.Default()
		if resolved.Err() != nil || !resolved.IsConcrete() {
			continue
		}
		encoded, err := resolved.MarshalJSON()
		if err != nil {
			continue
		}
		fields[r.fields[index]] = encoded
	}
	return fields
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
// are not inside a quoted string, as CUE does: a string may be raw
// (#"..."#), and commas inside brackets, braces or parentheses do not
// separate arguments.
func splitAttributeArguments(body string) []string {
	var arguments []string
	start := 0
	depth := 0
	for index := 0; index < len(body); index++ {
		switch char := body[index]; char {
		case '#', '"', '\'':
			index = endOfString(body, index)
		case '[', '{', '(':
			depth++
		case ']', '}', ')':
			if depth > 0 {
				depth--
			}
		case ',':
			if depth == 0 {
				arguments = append(arguments, body[start:index])
				start = index + 1
			}
		}
	}
	return append(arguments, body[start:])
}

// endOfString is the index of the last byte of the string that starts at
// index, which may be preceded by the `#`s of a raw string; index itself when
// no string starts there.
func endOfString(body string, index int) int {
	hashes := 0
	for index+hashes < len(body) && body[index+hashes] == '#' {
		hashes++
	}
	open := index + hashes
	if open >= len(body) || (body[open] != '"' && body[open] != '\'') {
		return index
	}
	quote := body[open]
	closing := string(quote) + strings.Repeat("#", hashes)
	for position := open + 1; position < len(body); position++ {
		if hashes == 0 && body[position] == '\\' {
			position++
			continue
		}
		if strings.HasPrefix(body[position:], closing) {
			return position + len(closing) - 1
		}
	}
	return len(body) - 1
}

// unquoteAttributeValue is the value of an attribute argument, which may be
// written bare or as a CUE string, raw strings included.
func unquoteAttributeValue(value string) string {
	if unquoted, err := literal.Unquote(value); err == nil {
		return unquoted
	}
	return value
}
