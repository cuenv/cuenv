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
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/load"
	"cuelang.org/go/cue/parser"
	"cuelang.org/go/cue/token"
	"cuelang.org/go/mod/modconfig"
	"cuelang.org/go/mod/module"
)

// Frozen instances contain syntax, not live evaluator state. Every operation
// reconstructs its own build graph and context. There are no Go handles to leak,
// no shared mutable CUE values, and no filesystem reads after freezing.
type frozenFile struct {
	Name            string          `json:"name"`
	Source          string          `json:"source"`
	Location        *frozenLocation `json:"location,omitempty"`
	LanguageVersion string          `json:"languageVersion,omitempty"`
}

type frozenNode struct {
	Dir, Root, Module, ImportPath, PkgName string
	User                                   bool
	ModuleFile                             json.RawMessage
	Files                                  []frozenFile
	Imports                                []int
	CanonicalID                            string            `json:"canonicalID,omitempty"`
	CanonicalImports                       map[string]string `json:"canonicalImports,omitempty"`
	DisplayPath                            string            `json:"displayPath,omitempty"`
	ModuleVersionPath                      string            `json:"moduleVersionPath,omitempty"`
	ModuleVersion                          string            `json:"moduleVersion,omitempty"`
	DirLocation                            *frozenLocation   `json:"dirLocation,omitempty"`
	RootLocation                           *frozenLocation   `json:"rootLocation,omitempty"`
}

type frozenGraph struct {
	Nodes []frozenNode `json:"nodes"`
	// Embedded holds what @embed attributes read in each loader filesystem.
	Embedded map[string]frozenEmbedFS `json:"embedded,omitempty"`
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
	graph, failure := freezeConsistentGraph(load.Config{Dir: dir, ModuleRoot: root, Package: options.Package, Registry: registry})
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

func freezeConsistentGraph(config load.Config) (*frozenGraph, string) {
	var previous []byte
	for attempt := 0; attempt < frozenLoadAttempts; attempt++ {
		graph, failure := freezeLoadedGraph(config)
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

func freezeLoadedGraph(config load.Config) (*frozenGraph, string) {
	// The CLI's OS loader needs to hide unrelated packages when they fail
	// parsing. An explicit loader FS or overlay is already the source of
	// truth and must not be replaced with a view of the host directory.
	if config.FS == nil && config.Overlay == nil {
		overlay, matched, err := packageFilterOverlay(config.Dir, config.Package, false)
		if err != nil || !matched {
			return nil, frozenFailure(ErrorCodeLoadInstance, "CUE snapshot package could not be loaded")
		}
		config.Overlay = overlay
	}
	// Capture precisely the bytes parsed by the loader, not a subsequent disk
	// read or a formatted AST. This preserves positions and prevents a source
	// change between loading and snapshotting from changing the selected input.
	sources := make(map[*ast.File]string)
	var sourceMutex sync.Mutex
	parse := config.ParseFile
	if parse == nil {
		parse = func(name string, source interface{}, parseConfig parser.Config) (*ast.File, error) {
			return parser.ParseFile(name, source, parseConfig)
		}
	}
	config.ParseFile = func(name string, source interface{}, parseConfig parser.Config) (*ast.File, error) {
		data, err := frozenSourceBytes(source)
		if err != nil {
			return nil, err
		}
		captured := string(data)
		file, err := parse(name, data, parseConfig)
		if err == nil {
			sourceMutex.Lock()
			sources[file] = captured
			sourceMutex.Unlock()
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

func frozenSourceBytes(source interface{}) ([]byte, error) {
	switch source := source.(type) {
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
	var instances []*build.Instance
	var hasExtern bool
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
		instances = append(instances, instance)
		graph.Nodes = append(graph.Nodes, frozenNode{})
		node := frozenNode{
			Dir: instance.Dir, Root: instance.Root, Module: instance.Module,
			ImportPath: instance.ImportPath, PkgName: instance.PkgName, User: instance.User,
			CanonicalID: instance.CanonicalID, DisplayPath: instance.DisplayPath,
			ModuleVersionPath: instance.ModuleVersion.Path(), ModuleVersion: instance.ModuleVersion.Version(),
			DirLocation: freezeLocation(instance.DirLoc), RootLocation: freezeLocation(instance.RootLoc),
		}
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
			hasExtern = hasExtern || strings.Contains(data, "@extern")
			entry := frozenFile{Name: file.Filename, Source: data}
			if len(file.Decls) != 0 {
				if tokenFile := file.Pos().File(); tokenFile != nil {
					entry.Location = freezeLocation(tokenFile.FSLoc())
					entry.LanguageVersion = tokenFile.LanguageVersion()
				}
			}
			node.Files = append(node.Files, entry)
			if instance.CanonicalImportPath != nil {
				for spec := range file.ImportSpecs() {
					importPath, err := strconv.Unquote(spec.Path.Value)
					if err != nil {
						return 0, err
					}
					if canonical := instance.CanonicalImportPath(importPath); canonical != importPath {
						if node.CanonicalImports == nil {
							node.CanonicalImports = make(map[string]string)
						}
						node.CanonicalImports[importPath] = canonical
					}
				}
			}
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
	if _, err := visit(root); err != nil {
		return graph, err
	}
	if hasExtern {
		graph.Embedded = captureEmbedded(root, instances)
	}
	return graph, nil
}

// build evaluates the snapshot with complete reads of internal references.
func (graph frozenGraph) build() (cue.Value, error) {
	return graph.evaluate(cuecontext.New(cuecontext.CompleteInternalReferences()))
}

func (graph frozenGraph) evaluate(runtime *cue.Context) (cue.Value, error) {
	instance, err := graph.instance()
	if err != nil {
		return cue.Value{}, err
	}
	value := runtime.BuildInstance(instance)
	if value.Err() != nil {
		return cue.Value{}, value.Err()
	}
	return value, nil
}

// references analyses what the fields at roots of value, its evaluation, depend on.
func (graph frozenGraph) references(value cue.Value, sources *[]valuePath, roots ...cue.Path) (*referenceGraph, error) {
	instance, err := graph.instance()
	if err != nil {
		return nil, err
	}
	return newReferenceGraph(instance, value, sources, roots)
}

func (graph frozenGraph) instance() (*build.Instance, error) {
	if len(graph.Nodes) == 0 {
		return nil, errors.New("empty frozen graph")
	}
	context := build.NewContext()
	instances := make([]*build.Instance, len(graph.Nodes))
	for index, node := range graph.Nodes {
		instance := context.NewInstance(node.Dir, nil)
		instance.Root, instance.Module, instance.ImportPath, instance.PkgName, instance.User = node.Root, node.Module, node.ImportPath, node.PkgName, node.User
		instance.CanonicalID, instance.DisplayPath = node.CanonicalID, node.DisplayPath
		if node.ModuleVersionPath != "" {
			var err error
			instance.ModuleVersion, err = module.NewVersion(node.ModuleVersionPath, node.ModuleVersion)
			if err != nil {
				return nil, errors.New("invalid frozen module version")
			}
		}
		instance.RootLoc = graph.restoreLocation(node.RootLocation, node.Root)
		instance.DirLoc = graph.restoreLocation(node.DirLocation, node.Dir)
		if node.CanonicalImports != nil {
			instance.CanonicalImportPath = func(importPath string) string {
				if canonical, ok := node.CanonicalImports[importPath]; ok {
					return canonical
				}
				return importPath
			}
		}
		if json.Unmarshal(node.ModuleFile, &instance.ModuleFile) != nil {
			return nil, errors.New("invalid frozen module")
		}
		for _, file := range node.Files {
			version := file.LanguageVersion
			if version == "" {
				version = instance.LanguageVersion()
			}
			parseConfig := parser.NewConfig(parser.ParseComments, parser.Version(version))
			syntax, err := parser.ParseFile(file.Name, file.Source, parseConfig)
			if err != nil {
				return nil, errors.New("invalid frozen syntax")
			}
			if len(syntax.Decls) != 0 {
				if tokenFile := syntax.Pos().File(); tokenFile != nil {
					loc := graph.restoreLocation(file.Location, file.Name)
					if loc.IsZero() {
						loc = token.FSLoc{FS: frozenEmbedFS{}, Path: "."}
					}
					tokenFile.SetFSLoc(loc)
				}
			}
			if instance.AddSyntax(syntax) != nil {
				return nil, errors.New("invalid frozen syntax")
			}
		}
		instances[index] = instance
	}
	for index, node := range graph.Nodes {
		for _, child := range node.Imports {
			if child < 0 || child >= len(instances) {
				return nil, errors.New("invalid frozen dependency")
			}
			instances[index].Imports = append(instances[index].Imports, instances[child])
		}
	}
	for _, instance := range instances {
		if instance.Complete() != nil {
			return nil, errors.New("invalid frozen instance")
		}
	}
	return instances[0], nil
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

// A frozenDeclaration is what a source declares itself to be, referenced or not.
type frozenDeclaration struct {
	Path          valuePath `json:"path"`
	PrimitiveType string    `json:"primitiveType"`
}

type frozenEvaluation struct {
	Value        interface{}         `json:"value"`
	References   []frozenReference   `json:"references"`
	Declarations []frozenDeclaration `json:"declarations"`
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
	value, err := graph.build()
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "Frozen CUE instance could not be evaluated")
	}
	references := make([]frozenReference, 0)
	if len(options.ReferencePaths) > 0 {
		roots := make([]cue.Path, 0, len(options.ReferencePaths))
		for _, path := range options.ReferencePaths {
			parsed, err := path.cuePath()
			if err != nil {
				return frozenFailure(ErrorCodeInvalidInput, "Invalid frozen CUE reference path")
			}
			roots = append(roots, parsed)
		}
		analysis, err := graph.references(value, options.SourcePaths, roots...)
		if err != nil {
			return frozenFailure(analysisFailureCode(err), "Frozen CUE references could not be inspected")
		}
		for _, root := range roots {
			found, err := analysis.frozenReferences(root)
			if err != nil {
				return frozenFailure(analysisFailureCode(err), "Frozen CUE references could not be inspected")
			}
			references = append(references, found...)
		}
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
	declarations := make([]frozenDeclaration, 0)
	if options.SourcePaths != nil {
		for _, path := range *options.SourcePaths {
			parsed, err := path.cuePath()
			if err != nil {
				return frozenFailure(ErrorCodeInvalidInput, "Invalid frozen CUE source path")
			}
			declarations = append(declarations, frozenDeclaration{Path: path, PrimitiveType: primitiveDeclaration(value.LookupPath(parsed))})
		}
	}
	encoded, err := json.Marshal(frozenEvaluation{Value: projected, References: references, Declarations: declarations})
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
