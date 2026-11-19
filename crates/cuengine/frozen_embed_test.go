package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/fstest"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/load"
	"cuelang.org/go/cue/parser"
	"cuelang.org/go/mod/modfile"
	"cuelang.org/go/mod/module"
)

func embedModule(t *testing.T) string {
	t.Helper()
	return writeCueModule(t, map[string]string{
		"cue.mod/module.cue": "module: \"example.com/bridge@v0\"\nlanguage: version: \"v0.18.0\"\n",
		"env.cue": `@extern(embed)
package app

import "example.com/bridge/schema"

file: _ @embed(file="a.json")
glob: _ @embed(glob="d/*.json")
data: _ @embed(file="b.bin", type=binary)
imported: schema.value
`,
		"a.json":           `{"v": "old"}`,
		"d/one.json":       `{"v": "old"}`,
		"b.bin":            "\xff\xfeold",
		"schema/types.cue": "@extern(embed)\npackage schema\nvalue: _ @embed(file=\"i.json\")\n",
		"schema/i.json":    `{"v": "old"}`,
	})
}

func TestFrozenEmbeddedOverlayBytes(t *testing.T) {
	dir := embedModule(t)
	cueSource := "@extern(embed)\npackage app\n\n// The overlay supplies both sources.\nvalue: _ @embed(file=\"b.bin\", type=binary)\n"
	embedBytes := []byte{0x00, 0xfe, '\r', '\n'}
	config := load.Config{
		Dir: dir, Package: "app",
		Overlay: map[string]load.Source{
			filepath.Join(dir, "env.cue"): load.FromString(cueSource),
			filepath.Join(dir, "b.bin"):   load.FromBytes(embedBytes),
		},
	}
	graph, failure := freezeConsistentGraph(config)
	if failure != "" {
		t.Fatal(failure)
	}
	if len(graph.Nodes[0].Files) != 1 || graph.Nodes[0].Files[0].Source != cueSource {
		t.Fatal("frozen CUE source differs from the overlay")
	}
	if err := os.RemoveAll(dir); err != nil {
		t.Fatal(err)
	}
	value, err := graph.build()
	if err != nil {
		t.Fatal(err)
	}
	got, err := value.LookupPath(cue.ParsePath("value")).Bytes()
	if err != nil || string(got) != string(embedBytes) {
		t.Fatalf("embedded overlay bytes = %v, %v", got, err)
	}
}

func TestFrozenEmbeddedVirtualLocation(t *testing.T) {
	fsys := fstest.MapFS{
		"project/cue.mod/module.cue": {Data: []byte("module: \"example.test/app@v0\"\nlanguage: version: \"v0.18.0\"\n")},
		"project/app/env.cue":        {Data: []byte("@extern(embed)\npackage app\nvalue: _ @embed(glob=\"data/*.json\")\n")},
		"project/app/data/one.json":  {Data: []byte("42")},
	}
	graph, failure := freezeConsistentGraph(load.Config{
		Dir: "/project/app", ModuleRoot: "/project", Package: "app", FS: fsys,
		FromFSPath: func(path string) string { return "memory:" + path },
	})
	if failure != "" {
		t.Fatal(failure)
	}
	clear(fsys)
	instance, err := graph.instance()
	if err != nil {
		t.Fatal(err)
	}
	if got := instance.RootLoc.String(); got != "memory:/project" {
		t.Fatalf("module location = %q", got)
	}
	if got := instance.DirLoc.String(); got != "memory:/project/app" {
		t.Fatalf("package location = %q", got)
	}
	if got := instance.Files[0].Pos().File().FSLoc().String(); got != "memory:/project/app/env.cue" {
		t.Fatalf("file location = %q", got)
	}
	value, err := graph.build()
	if err != nil {
		t.Fatal(err)
	}
	got, err := value.LookupPath(cue.MakePath(cue.Str("value"), cue.Str("data/one.json"))).Int64()
	if err != nil || got != 42 {
		t.Fatalf("embedded virtual file = %d, %v", got, err)
	}
}

func TestFrozenEmbeddedNestedModule(t *testing.T) {
	for _, attribute := range []string{`file="nested/value.json"`, `glob="nested/*.json"`} {
		t.Run(attribute, func(t *testing.T) {
			dir := writeCueModule(t, map[string]string{
				"cue.mod/module.cue":        "module: \"example.test/app@v0\"\nlanguage: version: \"v0.18.0\"\n",
				"env.cue":                   "@extern(embed)\npackage app\nvalue: _ @embed(" + attribute + ")\n",
				"nested/cue.mod/module.cue": "module: \"example.test/nested@v0\"\nlanguage: version: \"v0.18.0\"\n",
				"nested/value.json":         "42",
			})
			graph, failure := freezeConsistentGraph(load.Config{Dir: dir, Package: "app"})
			if failure != "" {
				t.Fatal(failure)
			}
			if err := os.RemoveAll(dir); err != nil {
				t.Fatal(err)
			}
			if _, err := graph.build(); err == nil || !strings.Contains(err.Error(), "in different module") {
				t.Fatalf("nested module error = %v", err)
			}
		})
	}
}

type frozenEmbedRegistry struct {
	dir  string
	dep  module.Version
	file *modfile.File
}

func (r frozenEmbedRegistry) ModFile(_ context.Context, version module.Version) (*modfile.File, error) {
	if version != r.dep {
		return nil, fmt.Errorf("unexpected module %s", version)
	}
	return r.file, nil
}

func (r frozenEmbedRegistry) Fetch(_ context.Context, version module.Version) (module.SourceLoc, error) {
	if version != r.dep {
		return module.SourceLoc{}, fmt.Errorf("unexpected module %s", version)
	}
	return module.SourceLoc{FS: module.OSDirFS(r.dir), Dir: "."}, nil
}

func (r frozenEmbedRegistry) ModuleVersions(_ context.Context, path string) ([]string, error) {
	if path != r.dep.Path() {
		return nil, fmt.Errorf("unexpected module path %s", path)
	}
	return []string{r.dep.Version()}, nil
}

func TestFrozenEmbeddedRegistryImport(t *testing.T) {
	const depModule = "module: \"example.test/dep@v1\"\nlanguage: version: \"v0.18.0\"\n"
	depDir := writeCueModule(t, map[string]string{
		"cue.mod/module.cue": depModule,
		"library/env.cue":    "@extern(embed)\npackage library\nvalue: _ @embed(file=\"value.json\")\n",
		"library/value.json": "42",
	})
	rootDir := writeCueModule(t, map[string]string{
		"cue.mod/module.cue": "module: \"example.test/app@v0\"\nlanguage: version: \"v0.18.0\"\ndeps: \"example.test/dep@v1\": {v: \"v1.2.3\", default: true}\n",
		"env.cue":            "package app\nimport \"example.test/dep/library\"\nvalue: library.value\n",
	})
	dep := module.MustNewVersion("example.test/dep@v1", "v1.2.3")
	mod, err := modfile.Parse([]byte(depModule), "cue.mod/module.cue")
	if err != nil {
		t.Fatal(err)
	}
	graph, failure := freezeConsistentGraph(load.Config{
		Dir: rootDir, Package: "app",
		Registry: frozenEmbedRegistry{dir: depDir, dep: dep, file: mod},
	})
	if failure != "" {
		t.Fatal(failure)
	}
	if err := os.RemoveAll(depDir); err != nil {
		t.Fatal(err)
	}
	instance, err := graph.instance()
	if err != nil {
		t.Fatal(err)
	}
	if imported := instance.LookupImport("example.test/dep/library"); imported == nil || imported.ModuleVersion != dep {
		t.Fatalf("unversioned registry import = %#v", imported)
	}
	value, err := graph.build()
	if err != nil {
		t.Fatal(err)
	}
	got, err := value.LookupPath(cue.ParsePath("value")).Int64()
	if err != nil || got != 42 {
		t.Fatalf("embedded registry dependency value = %d, %v", got, err)
	}
}

func embeddedValues(t *testing.T, graph frozenGraph) map[string]string {
	t.Helper()
	value, err := graph.build()
	if err != nil {
		t.Fatal(err)
	}
	result := make(map[string]string)
	for _, path := range []string{"file", "glob", "data", "imported"} {
		encoded, err := value.LookupPath(cue.ParsePath(path)).MarshalJSON()
		if err != nil {
			t.Fatal(err)
		}
		result[path] = string(encoded)
	}
	return result
}

// What a snapshot evaluates to is what was on disk when it was frozen, whatever
// happens to the files afterwards.
func TestFrozenEmbeddedFilesStayAsTheyWereFrozen(t *testing.T) {
	dir := embedModule(t)
	response := freezeInstanceResponse(frozenJSON(t, freezeOptions{ModuleRoot: dir, Directory: dir, Package: "app"}))
	var graph frozenGraph
	if err := json.Unmarshal(frozenPayload(t, response), &graph); err != nil {
		t.Fatal(err)
	}
	before := embeddedValues(t, graph)
	if !strings.Contains(before["data"], "//5vbGQ=") {
		t.Fatalf("binary data was not kept: %s", before["data"])
	}
	for name, content := range map[string]string{"a.json": `{"v": "new"}`, "d/two.json": `{"v": "new"}`, "b.bin": "new", "schema/i.json": `{"v": "new"}`} {
		if err := os.WriteFile(filepath.Join(dir, filepath.FromSlash(name)), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.Remove(filepath.Join(dir, "d/one.json")); err != nil {
		t.Fatal(err)
	}
	after := embeddedValues(t, graph)
	for path, value := range before {
		if after[path] != value {
			t.Errorf("%s changed from %s to %s", path, value, after[path])
		}
	}
	base, err := graph.build()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := graph.references(base, nil, cue.ParsePath("file")); err != nil {
		t.Errorf("analysis read from disk: %v", err)
	}
	if err := os.RemoveAll(dir); err != nil {
		t.Fatal(err)
	}
	if got := embeddedValues(t, graph); got["file"] != before["file"] {
		t.Errorf("a snapshot needs its directory: %s", got["file"])
	}
}

// An embedded file edited while freezing is covered by the same check as a CUE file.
func TestFrozenEmbeddedFileEditedWhileFreezing(t *testing.T) {
	dir := embedModule(t)
	path := filepath.Join(dir, "a.json")
	config := load.Config{Dir: dir, ModuleRoot: dir, Package: "app"}
	edits := 0
	config.ParseFile = func(name string, source interface{}, parseConfig parser.Config) (*ast.File, error) {
		file, err := parser.ParseFile(name, source, parseConfig)
		if strings.HasSuffix(name, "env.cue") && edits == 1 {
			if err := os.WriteFile(path, []byte(`{"v": "new"}`), 0o644); err != nil {
				t.Fatal(err)
			}
		}
		if strings.HasSuffix(name, "env.cue") {
			edits++
		}
		return file, err
	}
	graph, failure := freezeConsistentGraph(config)
	if failure != "" {
		t.Fatal(failure)
	}
	if got := embeddedValues(t, *graph)["file"]; !strings.Contains(got, "new") {
		t.Errorf("kept a file that had changed: %s", got)
	}
	config.ParseFile = func(name string, source interface{}, parseConfig parser.Config) (*ast.File, error) {
		file, err := parser.ParseFile(name, source, parseConfig)
		if strings.HasSuffix(name, "env.cue") {
			edits++
			if err := os.WriteFile(path, []byte(`{"v": `+strings.Repeat("1", edits)+`}`), 0o644); err != nil {
				t.Fatal(err)
			}
		}
		return file, err
	}
	if _, failure := freezeConsistentGraph(config); !strings.Contains(failure, "kept changing") {
		t.Errorf("expected a refusal for continuous edits: %s", failure)
	}
}

// A project with no embedded files holds none, and is not evaluated while freezing.
func TestFrozenWithoutEmbeddedFilesHoldsNone(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nx: 1\n"})
	if strings.Contains(snapshot, `"embedded"`) {
		t.Errorf("snapshot has an embedded section: %s", snapshot)
	}
}

// A conflict inside a let or an inline value used to be lost when an expression read it,
// so a wrong value reached the provider. A real value now fails as a conflict anywhere.
func TestFrozenRealValuesConflictingInsideALetAreRefused(t *testing.T) {
	for name, config := range map[string]string{
		"let interpolated": "let M = source & \"x\"\nconfig: value: \"\\(M)\"",
		"let compared":     "let M = source & \"x\"\nconfig: value: M == \"x\"",
		"let reversed":     "let M = \"x\" & source\nconfig: value: \"\\(M)\"",
		"let clause":       "config: {for k, v in {a: source} let i = v & \"x\" {value: \"\\(i)\"}}",
		"inline field":     "#F: {in: \"x\", out: \"\\(in)\"}\nconfig: value: (#F & {in: source}).out",
		"inline let":       "let P = {in: \"x\", out: \"\\(in)\"} & {in: source}\nconfig: value: P.out",
	} {
		t.Run(name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: string\n" + config + "\n"})
			var graph frozenGraph
			if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
				t.Fatal(err)
			}
			base, err := graph.build()
			if err != nil {
				t.Fatal(err)
			}
			verify := func(value string) error {
				return verifyFrozenInstance(base, frozenVerificationOptions{
					Fills:   []scalarFill{{Path: frozenPath("source"), Value: json.RawMessage(`"` + value + `"`)}},
					Roots:   []valuePath{frozenPath("config")},
					Mutable: []valuePath{frozenPath("config")},
				})
			}
			if err := verify("x"); err != nil {
				t.Errorf("the value that fits was refused: %v", err)
			}
			if err := verify("zzz"); err == nil {
				t.Error("a value conflicting inside it was accepted")
			}
		})
	}
}
