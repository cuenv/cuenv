package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/load"
	"cuelang.org/go/cue/parser"
)

func embedModule(t *testing.T) string {
	t.Helper()
	return writeCueModule(t, map[string]string{
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
