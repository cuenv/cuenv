package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"sync"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/load"
	"cuelang.org/go/cue/parser"
	"cuelang.org/go/mod/modconfig"
)

// Frozen instances contain syntax, not live evaluator state. Every operation
// reconstructs its own build graph and context. There are no Go handles to leak,
// no shared mutable CUE values, and no filesystem reads after freezing.
type frozenFile struct {
	Name   string `json:"name"`
	Source string `json:"source"`
}

type frozenNode struct {
	Dir, Root, Module, ImportPath, PkgName string
	User                                   bool
	ModuleFile                             json.RawMessage
	Files                                  []frozenFile
	Imports                                []int
}

type frozenGraph struct {
	Nodes []frozenNode `json:"nodes"`
}

type freezeOptions struct {
	ModuleRoot string `json:"moduleRoot"`
	Directory  string `json:"directory"`
	Package    string `json:"package"`
}

// No error at this boundary includes evaluator diagnostics, paths, source text,
// panic payloads or runtime scalars. Even a one-character string, bool or number
// can be sensitive; replacing registered strings cannot provide this guarantee.
func frozenFailure(code, message string) string {
	return createErrorResponse(code, message, nil)
}

//export cue_freeze_instance
func cue_freeze_instance(options *C.char) *C.char {
	return C.CString(freezeInstanceResponse(C.GoString(options)))
}

func freezeInstanceResponse(optionsJSON string) (result string) {
	defer func() {
		if recover() != nil {
			result = frozenFailure(ErrorCodePanicRecover, "CUE snapshot failed")
		}
	}()
	var options freezeOptions
	if json.Unmarshal([]byte(optionsJSON), &options) != nil || options.ModuleRoot == "" || options.Directory == "" || options.Package == "" {
		return frozenFailure(ErrorCodeInvalidInput, "Invalid CUE snapshot options")
	}
	root, err := filepath.Abs(options.ModuleRoot)
	if err != nil {
		return frozenFailure(ErrorCodeInvalidInput, "Invalid CUE module directory")
	}
	dir, err := filepath.Abs(options.Directory)
	if err != nil {
		return frozenFailure(ErrorCodeInvalidInput, "Invalid CUE instance directory")
	}
	registry, err := modconfig.NewRegistry(&modconfig.Config{Transport: http.DefaultTransport, ClientType: "cuengine"})
	if err != nil {
		return frozenFailure(ErrorCodeRegistryInit, "CUE snapshot registry could not be initialized")
	}
	graph, failure := freezeConsistentGraph(load.Config{Dir: dir, ModuleRoot: root, Package: options.Package, Registry: registry}, nil)
	if failure != "" {
		return failure
	}
	encoded, err := json.Marshal(graph)
	if err != nil {
		return frozenFailure(ErrorCodeJSONMarshal, "CUE snapshot could not be encoded")
	}
	return createSuccessResponse(string(encoded))
}

// Files are read one at a time, so an edit during loading can pair old bytes of one
// file with new bytes of another: a revision that never existed. Accept a graph only
// once two consecutive loads captured identical bytes.
const frozenLoadAttempts = 4

func freezeConsistentGraph(config load.Config, afterParse func(string)) (*frozenGraph, string) {
	var previous []byte
	for attempt := 0; attempt < frozenLoadAttempts; attempt++ {
		graph, failure := freezeLoadedGraph(config, afterParse)
		if failure != "" {
			return nil, failure
		}
		encoded, err := json.Marshal(graph)
		if err != nil {
			return nil, frozenFailure(ErrorCodeJSONMarshal, "CUE snapshot could not be encoded")
		}
		if previous != nil && bytes.Equal(previous, encoded) {
			return graph, ""
		}
		previous = encoded
	}
	return nil, frozenFailure(ErrorCodeLoadInstance, "CUE snapshot sources kept changing while being loaded")
}

func freezeLoadedGraph(config load.Config, afterParse func(string)) (*frozenGraph, string) {
	// Package filtering also reads the directory, so each load decides afresh.
	overlay, matched, err := packageFilterOverlay(config.Dir, config.Package, false)
	if err != nil || !matched {
		return nil, frozenFailure(ErrorCodeLoadInstance, "CUE snapshot package could not be loaded")
	}
	config.Overlay = overlay
	// Capture precisely the bytes parsed by the loader, not a subsequent disk
	// read or a formatted AST. This preserves positions and prevents a source
	// change between loading and snapshotting from changing the selected input.
	sources := make(map[*ast.File]string)
	var sourceMutex sync.Mutex
	config.ParseFile = func(name string, source interface{}, parseConfig parser.Config) (*ast.File, error) {
		data, err := frozenSourceBytes(name, source)
		if err != nil {
			return nil, err
		}
		file, err := parser.ParseFile(name, data, parseConfig)
		if err == nil {
			sourceMutex.Lock()
			sources[file] = string(data)
			sourceMutex.Unlock()
		}
		if afterParse != nil {
			afterParse(name)
		}
		return file, err
	}
	instances := load.Instances([]string{"."}, &config)
	if len(instances) != 1 || instances[0].Err != nil || instances[0].Incomplete {
		return nil, frozenFailure(ErrorCodeLoadInstance, "CUE snapshot package could not be loaded")
	}
	graph, err := freezeBuildGraph(instances[0], sources)
	if err != nil {
		return nil, frozenFailure(ErrorCodeBuildValue, "CUE snapshot could not be captured")
	}
	return graph, ""
}

func frozenSourceBytes(name string, source interface{}) ([]byte, error) {
	switch source := source.(type) {
	case nil:
		return os.ReadFile(name)
	case []byte:
		return source, nil
	case string:
		return []byte(source), nil
	case io.Reader:
		return io.ReadAll(source)
	default:
		return nil, errors.New("unsupported frozen source")
	}
}

func freezeBuildGraph(root *build.Instance, sources map[*ast.File]string) (*frozenGraph, error) {
	graph := &frozenGraph{}
	seen := make(map[*build.Instance]int)
	var visit func(*build.Instance) (int, error)
	visit = func(instance *build.Instance) (int, error) {
		if index, exists := seen[instance]; exists {
			return index, nil
		}
		if instance.Err != nil || instance.Incomplete {
			return 0, errors.New("invalid frozen dependency")
		}
		index := len(graph.Nodes)
		seen[instance] = index
		graph.Nodes = append(graph.Nodes, frozenNode{})
		node := frozenNode{Dir: instance.Dir, Root: instance.Root, Module: instance.Module, ImportPath: instance.ImportPath, PkgName: instance.PkgName, User: instance.User}
		var err error
		node.ModuleFile, err = json.Marshal(instance.ModuleFile)
		if err != nil {
			return 0, err
		}
		for _, file := range instance.Files {
			data, captured := sources[file]
			if !captured {
				return 0, errors.New("source was not captured by loader")
			}
			node.Files = append(node.Files, frozenFile{Name: file.Filename, Source: data})
		}
		for _, imported := range instance.Imports {
			child, err := visit(imported)
			if err != nil {
				return 0, err
			}
			node.Imports = append(node.Imports, child)
		}
		graph.Nodes[index] = node
		return index, nil
	}
	_, err := visit(root)
	return graph, err
}

// build evaluates the snapshot as CUE normally would. Exported values and filled
// consumers come from this mode. Anything inspecting provenance uses buildAnalysis.
func (graph frozenGraph) build() (cue.Value, error) {
	return graph.evaluate(cuecontext.New())
}

// Structure sharing makes an un-unified imported definition report references by
// its declared path. Provenance needs the path where the value was instantiated.
func (graph frozenGraph) buildAnalysis() (cue.Value, error) {
	return graph.evaluate(cuecontext.New(cuecontext.CUE_DEBUG("sharing=false")))
}

func (graph frozenGraph) evaluate(runtime *cue.Context) (cue.Value, error) {
	if len(graph.Nodes) == 0 {
		return cue.Value{}, errors.New("empty frozen graph")
	}
	context := build.NewContext()
	instances := make([]*build.Instance, len(graph.Nodes))
	for index, node := range graph.Nodes {
		instance := context.NewInstance(node.Dir, nil)
		instance.Root, instance.Module, instance.ImportPath, instance.PkgName, instance.User = node.Root, node.Module, node.ImportPath, node.PkgName, node.User
		if json.Unmarshal(node.ModuleFile, &instance.ModuleFile) != nil {
			return cue.Value{}, errors.New("invalid frozen module")
		}
		for _, file := range node.Files {
			if instance.AddFile(file.Name, file.Source) != nil {
				return cue.Value{}, errors.New("invalid frozen syntax")
			}
		}
		instances[index] = instance
	}
	for index, node := range graph.Nodes {
		for _, child := range node.Imports {
			if child < 0 || child >= len(instances) {
				return cue.Value{}, errors.New("invalid frozen dependency")
			}
			instances[index].Imports = append(instances[index].Imports, instances[child])
		}
	}
	for _, instance := range instances {
		if instance.Complete() != nil {
			return cue.Value{}, errors.New("invalid frozen instance")
		}
	}
	value := runtime.BuildInstance(instances[0])
	if value.Err() != nil {
		return cue.Value{}, value.Err()
	}
	return value, nil
}

type valueSelector struct {
	Field *string `json:"field,omitempty"`
	Index *int    `json:"index,omitempty"`
}

type valuePath []valueSelector

func (path valuePath) cuePath() (cue.Path, error) {
	selectors := make([]cue.Selector, 0, len(path))
	for _, selector := range path {
		switch {
		case selector.Field != nil && selector.Index == nil:
			selectors = append(selectors, cue.Str(*selector.Field))
		case selector.Index != nil && selector.Field == nil && *selector.Index >= 0:
			selectors = append(selectors, cue.Index(*selector.Index))
		default:
			return cue.Path{}, errors.New("invalid value path")
		}
	}
	return cue.MakePath(selectors...), nil
}

func typedPath(path cue.Path) valuePath {
	result := make(valuePath, 0, len(path.Selectors()))
	for _, selector := range path.Selectors() {
		switch selector.LabelType() {
		case cue.StringLabel:
			label := selector.Unquoted()
			result = append(result, valueSelector{Field: &label})
		case cue.IndexLabel:
			index := selector.Index()
			result = append(result, valueSelector{Index: &index})
		default:
			return nil
		}
	}
	return result
}

type frozenEvaluationOptions struct {
	ExportPaths    []valuePath  `json:"exportPaths"`
	ReferencePaths []valuePath  `json:"referencePaths"`
	SourcePaths    *[]valuePath `json:"sourcePaths"`
}

type frozenReference struct {
	Path          valuePath `json:"path"`
	Target        valuePath `json:"target"`
	PrimitiveType string    `json:"primitiveType"`
	Supported     bool      `json:"supported"`
	// Computed means CUE computes the value from its sources rather than carrying one.
	Computed bool `json:"computed"`
}

type frozenEvaluation struct {
	Value      interface{}       `json:"value"`
	References []frozenReference `json:"references"`
}

//export cue_evaluate_frozen
func cue_evaluate_frozen(snapshot, options *C.char) *C.char {
	return C.CString(evaluateFrozenResponse(C.GoString(snapshot), C.GoString(options)))
}

func evaluateFrozenResponse(snapshot, optionsJSON string) (result string) {
	defer func() {
		if recover() != nil {
			result = frozenFailure(ErrorCodePanicRecover, "Frozen CUE evaluation failed")
		}
	}()
	var graph frozenGraph
	var options frozenEvaluationOptions
	if json.Unmarshal([]byte(snapshot), &graph) != nil || json.Unmarshal([]byte(optionsJSON), &options) != nil {
		return frozenFailure(ErrorCodeInvalidInput, "Invalid frozen CUE evaluation options")
	}
	value, err := graph.buildAnalysis()
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "Frozen CUE instance could not be evaluated")
	}
	references := make([]frozenReference, 0)
	if len(options.ReferencePaths) > 0 {
		graph, err := newReferenceGraph(value, options.SourcePaths)
		if err != nil {
			return frozenFailure(ErrorCodeBuildValue, "Frozen CUE references could not be inspected")
		}
		for _, path := range options.ReferencePaths {
			parsed, err := path.cuePath()
			if err != nil {
				return frozenFailure(ErrorCodeInvalidInput, "Invalid frozen CUE reference path")
			}
			found, err := graph.frozenReferences(parsed)
			if err != nil {
				return frozenFailure(ErrorCodeBuildValue, "Frozen CUE references could not be inspected")
			}
			references = append(references, found...)
		}
	}
	// Exported values are consumed as ordinary input, so they come from a normal build.
	value, err = graph.build()
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "Frozen CUE instance could not be evaluated")
	}
	// Inspection is intentionally lenient and must not authorize consumer input.
	// specializeFrozenResponse performs strict validation before projection.
	projected := make(map[string]interface{})
	ordered := append([]valuePath{}, options.ExportPaths...)
	sort.SliceStable(ordered, func(i, j int) bool { return len(ordered[i]) < len(ordered[j]) })
	for _, path := range ordered {
		parsed, err := path.cuePath()
		if err != nil || len(path) == 0 || regularLabels(parsed) == nil {
			return frozenFailure(ErrorCodeInvalidInput, "Invalid frozen CUE export path")
		}
		field := value.LookupPath(parsed)
		if field.Exists() {
			inspected, err := inspectFrozenValue(field, parsed)
			if err != nil {
				return frozenFailure(ErrorCodeBuildValue, "Frozen CUE value could not be inspected")
			}
			setNested(projected, regularLabels(parsed), inspected)
		}
	}
	encoded, err := json.Marshal(frozenEvaluation{Value: projected, References: references})
	if err != nil {
		return frozenFailure(ErrorCodeJSONMarshal, "Frozen CUE evaluation could not be encoded")
	}
	return createSuccessResponse(string(encoded))
}

// Inspection permits incomplete leaves while preserving concrete JSON numbers
// exactly. Decode into interface{} would round them through float64.
func inspectFrozenValue(value cue.Value, path cue.Path) (interface{}, error) {
	if value.Kind() != cue.StructKind && value.Kind() != cue.ListKind {
		encoded, err := value.MarshalJSON()
		if err != nil {
			return nil, nil
		}
		return json.RawMessage(encoded), nil
	}
	object := make(map[string]interface{})
	items := make([]interface{}, 0)
	err := visitChildren(value, path, func(child cue.Value, childPath cue.Path) error {
		inspected, err := inspectFrozenValue(child, childPath)
		if err != nil {
			return err
		}
		selector := childPath.Selectors()[len(childPath.Selectors())-1]
		if selector.LabelType() == cue.IndexLabel {
			items = append(items, inspected)
		} else {
			object[selector.Unquoted()] = inspected
		}
		return nil
	})
	if value.Kind() == cue.ListKind {
		return items, err
	}
	return object, err
}
