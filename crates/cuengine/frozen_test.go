package main

import (
	"cuelang.org/go/cue/load"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"sync"
	"testing"

	"cuelang.org/go/cue"
)

func frozenJSON(t testing.TB, value interface{}) string {
	t.Helper()
	encoded, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return string(encoded)
}

func frozenPayload(t testing.TB, response string) json.RawMessage {
	t.Helper()
	var envelope testEnvelope
	if err := json.Unmarshal([]byte(response), &envelope); err != nil {
		t.Fatal(err)
	}
	if envelope.Error != nil {
		t.Fatalf("unexpected failure: %s", response)
	}
	return envelope.Ok
}

func frozenPath(path string) valuePath { return typedPath(cue.ParsePath(path)) }

func freezeTestModule(t testing.TB, files map[string]string) (string, string) {
	t.Helper()
	dir := writeCueModule(t, files)
	response := freezeInstanceResponse(frozenJSON(t, freezeOptions{ModuleRoot: dir, Directory: dir, Package: "app"}))
	return dir, string(frozenPayload(t, response))
}

func frozenFixture(t *testing.T) (string, string) {
	t.Helper()
	return freezeTestModule(t, map[string]string{
		"shared/types.cue": `package shared
import "strings"
#ID: string & strings.MinRunes(2)
`,
		"left/types.cue": `package left
import "example.com/bridge/shared"
#Config: {id: shared.#ID, literal: "ok", flag: bool, amount: number}
`,
		"right/types.cue": `package right
import "example.com/bridge/shared"
#ID: shared.#ID
`,
		"env.cue": `package app
import (
 "example.com/bridge/left"
 "example.com/bridge/right"
)
let Alias = source.id
fieldAlias: source.id
source: {id: string, flag: bool, amount: number}
config: {
 entries: [left.#Config & {id: Alias, literal: fieldAlias, flag: source.flag, amount: source.amount}]
 "0": right.#ID & source.id
}
`,
	})
}

func fixtureDeferred() []deferredValue {
	return []deferredValue{
		{Path: frozenPath("config.entries[0].id"), Target: frozenPath("source.id")},
		{Path: frozenPath("config.entries[0].literal"), Target: frozenPath("source.id")},
		{Path: frozenPath("config.entries[0].flag"), Target: frozenPath("source.flag")},
		{Path: frozenPath("config.entries[0].amount"), Target: frozenPath("source.amount")},
		{Path: frozenPath(`config."0"`), Target: frozenPath("source.id")},
	}
}

func TestFrozenSnapshotImportedClosedSchemaAndSourceDeletion(t *testing.T) {
	dir, snapshot := frozenFixture(t)
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		t.Fatal(err)
	}
	if len(graph.Nodes) != 4 {
		t.Fatalf("diamond import graph duplicated nodes: %d", len(graph.Nodes))
	}
	sharedCount := 0
	for _, node := range graph.Nodes {
		if node.PkgName == "shared" {
			sharedCount++
		}
		for _, file := range node.Files {
			original, err := os.ReadFile(file.Name)
			if err != nil || string(original) != file.Source {
				t.Fatalf("original source/filename was not preserved: %s", file.Name)
			}
		}
	}
	if sharedCount != 1 {
		t.Fatalf("shared dependency count: %d", sharedCount)
	}
	if err := os.WriteFile(filepath.Join(dir, "env.cue"), []byte("package app\nconfig: wrong: true\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.RemoveAll(dir); err != nil {
		t.Fatal(err)
	}
	options := specializationOptions{Projection: frozenPath("config"), Deferred: fixtureDeferred()}
	initial := frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, options)))
	var result struct {
		Value      map[string]interface{} `json:"value"`
		Unresolved []deferredValue        `json:"unresolved"`
	}
	if err := json.Unmarshal(initial, &result); err != nil {
		t.Fatal(err)
	}
	if len(result.Unresolved) != 5 || result.Value["0"] != nil {
		t.Fatalf("lost deferred provenance: %s", initial)
	}
	entry := result.Value["entries"].([]interface{})[0].(map[string]interface{})
	if entry["literal"] != nil {
		t.Fatalf("unknown source narrowed to literal was exported as known: %s", initial)
	}
	options.Fills = []scalarFill{{Path: frozenPath("source.id"), Value: json.RawMessage(`"ok"`)}}
	partial := frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, options)))
	if err := json.Unmarshal(partial, &result); err != nil || len(result.Unresolved) != 2 {
		t.Fatalf("partial specialization: %s (%v)", partial, err)
	}
	options.Fills = append(options.Fills,
		scalarFill{Path: frozenPath("source.flag"), Value: json.RawMessage("true")},
		scalarFill{Path: frozenPath("source.amount"), Value: json.RawMessage("9007199254740993.1234567890123456789")})
	complete := frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, options)))
	if !strings.Contains(string(complete), "9007199254740993.1234567890123456789") {
		t.Fatalf("numeric precision lost: %s", complete)
	}
	options.Fills[0].Value = json.RawMessage(`"xy"`)
	if response := specializeFrozenResponse(snapshot, frozenJSON(t, options)); !strings.Contains(response, `"error"`) {
		t.Fatalf("literal constraint was discarded: %s", response)
	}
	options.Fills = nil
	fresh := frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, options)))
	if err := json.Unmarshal(fresh, &result); err != nil || len(result.Unresolved) != 5 {
		t.Fatalf("specializations accumulated fills: %s", fresh)
	}
}

func TestFrozenTypedReferences(t *testing.T) {
	_, snapshot := frozenFixture(t)
	payload := frozenPayload(t, evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ExportPaths: []valuePath{frozenPath("config")}, ReferencePaths: []valuePath{frozenPath("config")}})))
	var result frozenEvaluation
	if err := json.Unmarshal(payload, &result); err != nil {
		t.Fatal(err)
	}
	for _, expected := range fixtureDeferred() {
		found := false
		for _, actual := range result.References {
			if reflect.DeepEqual(actual.Path, expected.Path) && reflect.DeepEqual(actual.Target, expected.Target) && actual.Supported {
				found = true
			}
		}
		if !found {
			t.Fatalf("missing typed reference %s: %s", frozenJSON(t, expected), payload)
		}
	}
}

func TestFrozenStrictDeferredValidation(t *testing.T) {
	for _, test := range []struct{ name, extra string }{
		{"unrelated incomplete", "other: string"},
		{"required field", "other!: string"},
		{"open list", "other: [...string]"},
		{"arithmetic", "other: source.number + 1"},
		{"interpolation", `other: "prefix-\(source.id)"`},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: {id: string, number: number}\nconfig: {id: source.id, " + test.extra + "}\n"})
			options := specializationOptions{Projection: frozenPath("config"), Deferred: []deferredValue{{Path: frozenPath("config.id"), Target: frozenPath("source.id")}}}
			if response := specializeFrozenResponse(snapshot, frozenJSON(t, options)); !strings.Contains(response, `"error"`) {
				t.Fatalf("unrelated failure was excused: %s", response)
			}
		})
	}
	_, snapshot := frozenFixture(t)
	for _, entries := range [][]deferredValue{nil, {{Path: frozenPath("config.entries"), Target: frozenPath("source.id")}}, {{Path: frozenPath("config.entries[0].id"), Target: frozenPath("source.flag")}}} {
		options := specializationOptions{Projection: frozenPath("config"), Deferred: entries}
		if response := specializeFrozenResponse(snapshot, frozenJSON(t, options)); !strings.Contains(response, `"error"`) {
			t.Fatalf("inexact exemption accepted: %s", response)
		}
	}
}

func TestFrozenSecretSafeFailures(t *testing.T) {
	for _, scalar := range []string{`"x"`, `"xy"`, `false`, `1`, `9007199254740993.123456789`} {
		_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: value: string\nconfig: value: source.value & \"sentinel\"\n"})
		options := specializationOptions{Projection: frozenPath("config"), Fills: []scalarFill{{Path: frozenPath("source.value"), Value: json.RawMessage(scalar)}}}
		response := specializeFrozenResponse(snapshot, frozenJSON(t, options))
		var result testEnvelope
		if err := json.Unmarshal([]byte(response), &result); err != nil || result.Error == nil {
			t.Fatalf("expected safe failure: %s", response)
		}
		if result.Error.Message != "CUE specialization did not satisfy the selected constraints" || result.Error.Hint != nil {
			t.Fatalf("value-bearing diagnostic escaped: %s", response)
		}
	}
}

func TestFrozenScalarConstraintsAndTypedSourcePaths(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
#Root: {
 source: {items: [{"0": string}], amount: number, enabled: bool}
 config: {id: string & =~"^ok", amount: >0 & <10, enabled: true}
}
#Root
source: items: [{"0": string}]
config: {id: source.items[0]."0", amount: source.amount, enabled: source.enabled}
`})
	options := specializationOptions{
		Projection: frozenPath("config"),
		Fills: []scalarFill{
			{Path: frozenPath(`source.items[0]."0"`), Value: json.RawMessage(`"ok"`)},
			{Path: frozenPath("source.amount"), Value: json.RawMessage("5.125")},
			{Path: frozenPath("source.enabled"), Value: json.RawMessage("true")},
		},
	}
	frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, options)))
	for _, test := range []struct {
		index int
		value string
	}{{0, `"x"`}, {1, "-1"}, {2, "false"}} {
		previous := options.Fills[test.index].Value
		options.Fills[test.index].Value = json.RawMessage(test.value)
		response := specializeFrozenResponse(snapshot, frozenJSON(t, options))
		if !strings.Contains(response, `"error"`) {
			t.Fatalf("scalar constraint was lost: %s", response)
		}
		options.Fills[test.index].Value = previous
	}
	// Existing-source-only and primitive-only fills cannot extend closed inputs.
	for _, fill := range []scalarFill{
		{Path: frozenPath("source.missing"), Value: json.RawMessage(`"ok"`)},
		{Path: frozenPath("source.items"), Value: json.RawMessage(`"ok"`)},
		{Path: frozenPath("source.amount"), Value: json.RawMessage("null")},
		{Path: frozenPath("source.amount"), Value: json.RawMessage("[]")},
	} {
		options.Fills = []scalarFill{fill}
		if response := specializeFrozenResponse(snapshot, frozenJSON(t, options)); !strings.Contains(response, `"error"`) {
			t.Fatalf("invalid fill was accepted: %s", response)
		}
	}
}

func TestFrozenFreshContextsConcurrentAndReleased(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: id: string\nconfig: id: source.id\n"})
	options := frozenJSON(t, specializationOptions{Projection: frozenPath("config"), Fills: []scalarFill{{Path: frozenPath("source.id"), Value: json.RawMessage(`"ok"`)}}})
	// Warm global CUE caches before comparing retained memory. This is a bounded
	// regression witness, not a hard memory bound or proof of cancellation.
	frozenPayload(t, specializeFrozenResponse(snapshot, options))
	runtime.GC()
	var before, after runtime.MemStats
	runtime.ReadMemStats(&before)
	var workers sync.WaitGroup
	for range 8 {
		workers.Go(func() {
			for range 20 {
				frozenPayload(t, specializeFrozenResponse(snapshot, options))
			}
		})
	}
	workers.Wait()
	runtime.GC()
	runtime.ReadMemStats(&after)
	if after.HeapAlloc > before.HeapAlloc+8*1024*1024 {
		t.Fatalf("fresh contexts retained excessive memory after GC: before=%d after=%d", before.HeapAlloc, after.HeapAlloc)
	}
	t.Logf("160 concurrent fresh specializations, retained heap before=%d after=%d", before.HeapAlloc, after.HeapAlloc)
}

func TestFrozenStrictSelectedSubtrees(t *testing.T) {
	for _, test := range []struct {
		name, extra string
		valid       bool
	}{
		{"valid", "", true},
		{"ordinary incomplete", "state: token: string", false},
		{"required state field", "state: token!: string", false},
		{"open consumer list", "consumer: extra: [...string]", false},
		{"unrelated consumer incomplete", "consumer: extra: string", false},
		{"refined abstract source", `producer: outputs: id: =~"^ok"`, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
state: driver: "local"
producer: outputs: {id: string, amount: number, enabled: bool}
consumer: entries: [{id: producer.outputs.id & "ok"}]
unused: token: string
` + test.extra + "\n"})
			options := frozenValidationOptions{
				ConcretePaths: []valuePath{frozenPath("state"), frozenPath("producer"), frozenPath("consumer")},
				AbstractPaths: []valuePath{frozenPath("producer.outputs")},
				Deferred:      []deferredValue{{Path: frozenPath("consumer.entries[0].id"), Target: frozenPath("producer.outputs.id")}},
			}
			response := validateFrozenResponse(snapshot, frozenJSON(t, options))
			var result testEnvelope
			if err := json.Unmarshal([]byte(response), &result); err != nil {
				t.Fatal(err)
			}
			if (result.Error == nil) != test.valid {
				t.Fatalf("valid=%t: %s", test.valid, response)
			}
			if test.valid {
				options.Deferred[0].Target = frozenPath("producer.outputs.amount")
				if response := validateFrozenResponse(snapshot, frozenJSON(t, options)); !strings.Contains(response, `"error"`) {
					t.Fatalf("wrong deferred source was accepted: %s", response)
				}
			}
		})
	}
}

func TestFrozenIndexedSourceProvenanceSurvivesNarrowing(t *testing.T) {
	for _, expression := range []string{
		`(source[0].number + 1) & 42`,
		`*"constant" | source[0].id`,
		`"prefix-\(source[0].id)" & "prefix-ok"`,
	} {
		_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: [{number: number, id: string}]\nconfig: value: " + expression + "\n"})
		options := specializationOptions{Projection: frozenPath("config")}
		if response := specializeFrozenResponse(snapshot, frozenJSON(t, options)); !strings.Contains(response, `"error"`) {
			t.Fatalf("unsupported indexed expression lost provenance: %s", response)
		}
	}
}

func TestFrozenOutputDependentStructureFailsClosed(t *testing.T) {
	for _, config := range []string{
		`value: *"fallback" | source.id`,
		`value: *[] | [for item in [source.id] {item}]`,
		`value: *0 | [1, 2][source.index]`,
		`value: *[] | [for values in [[1, 2]] {values[source.index]}]`,
		`value: *{} | {"\(source.id)": "literal"}`,
		`if source.enabled {value: "literal"}`,
		`if (source.id & "ok") == "ok" {value: "literal"}`,
		`for item in [source.id & "ok"] {"\(item)": "literal"}`,
		`"\(source.id & "ok")": "literal"`,
		`value: [1, 2][source.index & 0]`,
	} {
		t.Run(config, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: {id: string, index: number, enabled: bool}\nconfig: {" + config + "}\n"})
			options := frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}}
			response := evaluateFrozenResponse(snapshot, frozenJSON(t, options))
			var envelope testEnvelope
			if err := json.Unmarshal([]byte(response), &envelope); err != nil {
				t.Fatal(err)
			}
			if envelope.Error != nil {
				return
			}
			validation := frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("config")}}
			if response := validateFrozenResponse(snapshot, frozenJSON(t, validation)); !strings.Contains(response, `"error"`) {
				t.Fatalf("strict validation accepted output-dependent structure: %s", response)
			}
			if response := specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config")})); !strings.Contains(response, `"error"`) {
				t.Fatalf("specialization accepted output-dependent structure: %s", response)
			}
			var inspection frozenEvaluation
			if err := json.Unmarshal(envelope.Ok, &inspection); err != nil {
				t.Fatal(err)
			}
			for _, reference := range inspection.References {
				if reference.PrimitiveType != "" && !reference.Supported {
					return
				}
			}
			t.Fatalf("output-dependent structure lost provenance: %s", response)
		})
	}
}

func TestFrozenUnrelatedDynamicStructureRemainsSupported(t *testing.T) {
	for _, config := range []string{
		`if true {value: "literal"}`,
		`for item in [fixed] {"\(item)": "literal"}`,
		`"\(fixed)": "literal"`,
		`value: [1, 2][index]`,
		`value: *[] | [for item in [fixed] {item}]`,
	} {
		t.Run(config, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nfixed: \"ok\"\nindex: 0\nsource: id: string\nconfig: {" + config + ", id: source.id}\n"})
			entry := deferredValue{Path: frozenPath("config.id"), Target: frozenPath("source.id")}
			validation := frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("config")}, Deferred: []deferredValue{entry}}
			frozenPayload(t, validateFrozenResponse(snapshot, frozenJSON(t, validation)))
			options := specializationOptions{Projection: frozenPath("config"), Deferred: []deferredValue{entry}}
			frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, options)))
		})
	}
}

func TestFrozenComprehensionFallbackVisibility(t *testing.T) {
	for _, test := range []struct {
		name, declarations string
		valid              bool
	}{
		{"visible fallback not selected", `config: {if (source.enabled & true) {_hidden: true} else {value: "literal"}}`, false},
		{"visible fallback selected", `config: {if (source.enabled & false) {_hidden: true} else {value: "literal"}}`, false},
		{"string condition selects fallback", `config: {if (source.id & "ok") != "ok" {_hidden: true} else {value: "literal"}}`, false},
		{"nested visible fallback", `config: {if false {_hidden: true} else {if (source.enabled & true) {_hidden: true} else {value: "literal"}}}`, false},
		{"hidden fallback alias selected", `_alias: string
if (source.enabled & false) {_alias: "primary"} else {_alias: "fallback"}
config: value: _alias`, false},
		{"hidden fallback alias not selected", `_alias: string
if (source.enabled & true) {_alias: "primary"} else {_alias: "fallback"}
config: value: _alias`, false},
		{"constant fallback selected", `config: {if false {_hidden: true} else {value: "literal"}}`, true},
		{"constant fallback not selected", `config: {if true {_hidden: true} else {value: "literal"}}`, true},
		{"constant hidden fallback alias", `_alias: string
if false {_alias: "primary"} else {_alias: "fallback"}
config: value: _alias`, true},
		{"output controlled hidden branches", `config: {if (source.enabled & true) {_hidden: true} else {_hidden: false}, value: "literal"}`, false},
		{"output controlled hidden primary without fallback", `config: {if (source.enabled & true) {_hidden: true}, value: "literal"}`, true},
		{"constant hidden branches", `config: {if true {_hidden: true} else {_hidden: false}, value: "literal"}`, true},
		{"constant hidden primary without fallback", `config: {if true {_hidden: true}, value: "literal"}`, true},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{
				"cue.mod/module.cue": "module: \"example.com/bridge@v0\"\nlanguage: version: \"v0.16.0\"\n",
				"env.cue":            "@experiment(try)\npackage app\nsource: {enabled: bool, id: string}\n" + test.declarations + "\n",
			})
			inspected := evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}}))
			validation := validateFrozenResponse(snapshot, frozenJSON(t, frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("config")}}))
			specialization := specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config")}))
			if test.valid {
				frozenPayload(t, inspected)
				frozenPayload(t, validation)
				frozenPayload(t, specialization)
				return
			}
			for operation, response := range map[string]string{"validation": validation, "specialization": specialization} {
				if !strings.Contains(response, `"error"`) {
					t.Fatalf("%s accepted output-controlled fallback: %s", operation, response)
				}
			}
			var envelope testEnvelope
			if err := json.Unmarshal([]byte(inspected), &envelope); err != nil {
				t.Fatal(err)
			}
			if envelope.Error != nil {
				return
			}
			var evaluation frozenEvaluation
			if err := json.Unmarshal(envelope.Ok, &evaluation); err != nil {
				t.Fatal(err)
			}
			for _, reference := range evaluation.References {
				if reference.PrimitiveType != "" && !reference.Supported {
					return
				}
			}
			t.Fatalf("inspection lost output-controlled fallback provenance: %s", inspected)
		})
	}
}

func TestFrozenSlicesPreserveOrdinaryCUEAndRejectRuntimeBounds(t *testing.T) {
	for _, expression := range []string{
		`["a", "b"][:1]`, `["a", "b"][0:]`, `["a", "b"][:]`, `["a", "b"][0:1]`,
		`(["a", "b"][:1])`, `["a", "b"][:1] & ["a"]`, `["a"] & ["a", "b"][:1]`,
		`values[:end]`, `values[start:]`, `alias`,
	} {
		t.Run(expression, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nvalues: [\"a\", \"b\"]\nstart: 0\nend: 1\nalias: values[:end]\nsource: id: string\nconfig: {id: source.id, value: " + expression + "}\n"})
			entry := deferredValue{Path: frozenPath("config.id"), Target: frozenPath("source.id")}
			frozenPayload(t, evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}})))
			frozenPayload(t, validateFrozenResponse(snapshot, frozenJSON(t, frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("config")}, Deferred: []deferredValue{entry}})))
			frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config"), Deferred: []deferredValue{entry}})))
		})
	}
	for _, expression := range []string{
		`["a", "b"][:(source.index & 1)]`, `["a", "b"][(source.index & 0):]`,
		`["a", "b"][(source.index & 0):(source.index & 1)]`,
	} {
		t.Run(expression, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: index: number\nconfig: value: " + expression + "\n"})
			for operation, response := range map[string]string{
				"validation":     validateFrozenResponse(snapshot, frozenJSON(t, frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("config")}})),
				"specialization": specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config")})),
			} {
				if !strings.Contains(response, `"error"`) || strings.Contains(response, `"PANIC_RECOVER"`) {
					t.Fatalf("%s must reject runtime slice bounds without a panic: %s", operation, response)
				}
			}
		})
	}
}

func TestFrozenStructuralControlsRespectAliasesAndScopes(t *testing.T) {
	for _, test := range []struct {
		name, prefix, config string
		valid                bool
	}{
		{"constant let", "let label = fixed", `"\(label)": "literal"`, true},
		{"output let", "let label = source.id", `"\(label & "ok")": "literal"`, false},
		{"output let retains outer scope", "let label = source.id", `source: {id: "ok"}, "\(label & "ok")": "literal"`, false},
		{"constant alias", `template: {"\(fixed)": "literal"}`, "template", true},
		{"output alias", `template: {"\(source.id & "ok")": "literal"}`, "template", false},
		{"constant imported function", `import "strings"`, `"\(strings.ToLower(fixed))": "literal"`, true},
		{"output imported function", `import "strings"`, `"\(strings.ToLower(source.id & "ok"))": "literal"`, false},
		{"local constant shadows source field", "", `source: {id: "ok"}, "\(source.id)": "literal"`, true},
	} {
		t.Run(test.name, func(t *testing.T) {
			config := "{" + test.config + "}"
			if test.config == "template" {
				config = test.config
			}
			body := "package app\n" + test.prefix + "\nfixed: \"ok\"\nsource: id: string\nconfig: " + config + "\n"
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": body})
			options := specializationOptions{Projection: frozenPath("config")}
			response := specializeFrozenResponse(snapshot, frozenJSON(t, options))
			if strings.Contains(response, `"error"`) == test.valid {
				t.Fatalf("valid=%t: %s", test.valid, response)
			}
		})
	}
}

func TestFrozenImportedHiddenSemanticChecks(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{
		"schema/types.cue": `package schema
import "strings"
#Configuration: {
 state!: {local: {url: string}}
 migration?: {local: {url: string}}
 if migration != _|_ {
  let targets = [for field, _ in migration {field}]
  if len(targets) != 1 {_unresolved: migration: error("one target required")}
 }
 resources: [string]: {type: string, outputs?: [string]: string, configuration?: {...}}
 if state != _|_ {
  let backends = [for field, _ in state {field}]
  if len(backends) != 1 {_unresolved: state: error("one backend required")}
 }
 for name, resource in resources {
  if strings.SplitN(resource.type, "_", 2)[0] != "fake" {_unresolved: (name): error("unknown provider")}
 }
}
`,
		"env.cue": `package app
import "example.com/bridge/schema"
config: schema.#Configuration & {
 state: local: url: "http://localhost"
 resources: {producer: {type: "fake_source", outputs: id: string}, consumer: {type: "fake_consumer", configuration: id: config.resources.producer.outputs.id}}
}
`,
	})
	response := evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}}))
	frozenPayload(t, response)
	entry := deferredValue{Path: frozenPath("config.resources.consumer.configuration.id"), Target: frozenPath("config.resources.producer.outputs.id")}
	frozenPayload(t, validateFrozenResponse(snapshot, frozenJSON(t, frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("config")}, AbstractPaths: []valuePath{frozenPath("config.resources.producer.outputs")}, Deferred: []deferredValue{entry}})))
}

func TestFrozenHiddenDependenciesAndValidation(t *testing.T) {
	for _, declaration := range []string{
		`_alias: *"fallback" | source.id`,
		`if (source.id & "ok") == "ok" {_alias: "literal"}`,
		`_alias: {"\(source.id & "ok")": "literal"}`,
	} {
		_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: id: string\n" + declaration + "\nconfig: value: _alias\n"})
		if response := specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config")})); !strings.Contains(response, `"error"`) {
			t.Fatalf("hidden dependency reached exported data without provenance: %s", response)
		}
	}
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
source: id: string
_alias: source.id & !="bad"
config: id: _alias
`})
	entry := deferredValue{Path: frozenPath("config.id"), Target: frozenPath("source.id")}
	options := specializationOptions{Projection: frozenPath("config"), Deferred: []deferredValue{entry}}
	frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, options)))
	options.Fills = []scalarFill{{Path: frozenPath("source.id"), Value: json.RawMessage(`"ok"`)}}
	frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, options)))
	options.Fills[0].Value = json.RawMessage(`"bad"`)
	if response := specializeFrozenResponse(snapshot, frozenJSON(t, options)); !strings.Contains(response, `"error"`) {
		t.Fatalf("post-fill hidden semantic error was ignored: %s", response)
	}
}

// Editing two files while they load must not freeze a revision that never existed.
func TestFrozenSnapshotRefusesMixedRevisions(t *testing.T) {
	version := func(name string) map[string]string {
		return map[string]string{
			"a.cue": "package app\nfirst: \"" + name + "\"\n",
			"b.cue": "package app\nsecond: \"" + name + "\"\n",
		}
	}
	dir := writeCueModule(t, version("old"))
	write := func(files map[string]string) {
		t.Helper()
		for name, contents := range files {
			if err := os.WriteFile(filepath.Join(dir, name), []byte(contents), 0o644); err != nil {
				t.Fatal(err)
			}
		}
	}
	config := load.Config{Dir: dir, ModuleRoot: dir, Package: "app"}
	// The loader reads each file twice, imports first. Edit within the full pass.
	reads := 0
	editOnce := func(name string) {
		if strings.HasSuffix(name, "a.cue") {
			reads++
			if reads == 2 {
				write(version("new"))
			}
		}
	}
	mixed, failure := freezeLoadedGraph(config, editOnce)
	if failure != "" {
		t.Fatal(failure)
	}
	sources := strings.Join([]string{mixed.Nodes[0].Files[0].Source, mixed.Nodes[0].Files[1].Source}, "")
	if !strings.Contains(sources, "old") || !strings.Contains(sources, "new") {
		t.Fatalf("a single load should reproduce the mixed revision: %s", sources)
	}

	write(version("old"))
	reads = 0
	graph, failure := freezeConsistentGraph(config, editOnce)
	if failure != "" {
		t.Fatal(failure)
	}
	sources = strings.Join([]string{graph.Nodes[0].Files[0].Source, graph.Nodes[0].Files[1].Source}, "")
	if strings.Contains(sources, "old") || !strings.Contains(sources, "new") {
		t.Fatalf("consistent load kept a mixed revision: %s", sources)
	}

	counter := 0
	if _, failure := freezeConsistentGraph(config, func(name string) {
		if strings.HasSuffix(name, "a.cue") {
			counter++
			write(version(fmt.Sprintf("edit-%d", counter)))
		}
	}); !strings.Contains(failure, "kept changing") {
		t.Fatalf("expected a refusal for continuous edits: %s", failure)
	}
}

// A value computed from sources stays unknown until all of them are known, and CUE
// computes it from the real values, with its constraints.
func TestFrozenComputedReferences(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
import "strings"
source: {a: string, b: string, n: number}
config: {
 interpolated: "x-\(source.a)-\(source.b)"
 upper: strings.ToUpper(source.a)
 constrained: "x-\(source.a)" & =~"^x-ok"
 sum: source.n + 1
 plain: source.a
}
`})
	path := func(field string) valuePath { return frozenPath("config." + field) }
	source := func(field string) valuePath { return frozenPath("source." + field) }
	references := []deferredValue{
		{Path: path("interpolated"), Target: source("a")},
		{Path: path("interpolated"), Target: source("b")},
		{Path: path("upper"), Target: source("a")},
		{Path: path("constrained"), Target: source("a")},
		{Path: path("sum"), Target: source("n")},
		{Path: path("plain"), Target: source("a")},
	}
	var evaluation frozenEvaluation
	inspection := frozenPayload(t, evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}})))
	if err := json.Unmarshal(inspection, &evaluation); err != nil {
		t.Fatal(err)
	}
	if len(evaluation.References) != len(references) {
		t.Fatalf("references: %s", inspection)
	}
	for _, reference := range evaluation.References {
		consumer, _ := reference.Path.cuePath()
		if !reference.Supported || reference.Computed == (consumer.String() == "config.plain") {
			t.Fatalf("%s: %s", consumer, inspection)
		}
	}
	frozenPayload(t, validateFrozenResponse(snapshot, frozenJSON(t, frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("config")}, AbstractPaths: []valuePath{frozenPath("source")}, Deferred: references})))

	fill := func(field, value string) scalarFill {
		return scalarFill{Path: source(field), Value: json.RawMessage(value)}
	}
	specialize := func(fills []scalarFill, deferred []deferredValue) string {
		return specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config"), Fills: fills, Deferred: deferred}))
	}
	// Nothing known: every computed value is a placeholder.
	var result specializationResult
	if err := json.Unmarshal(frozenPayload(t, specialize(nil, references)), &result); err != nil {
		t.Fatal(err)
	}
	if len(result.Unresolved) != len(references) {
		t.Fatalf("unresolved: %+v", result)
	}
	// One of two sources known: still unknown, with the other named.
	known := append([]deferredValue{}, references[1], references[4])
	response := specialize([]scalarFill{fill("a", `"ok"`)}, known)
	if err := json.Unmarshal(frozenPayload(t, response), &result); err != nil {
		t.Fatal(err)
	}
	projected, _ := result.Value.(map[string]any)
	if projected["interpolated"] != nil || projected["upper"] != "OK" || projected["constrained"] != "x-ok" || projected["sum"] != nil || projected["plain"] != "ok" {
		t.Fatalf("partly known: %s", response)
	}
	// All known: CUE computes every value.
	response = specialize([]scalarFill{fill("a", `"ok"`), fill("b", `"b"`), fill("n", "41")}, nil)
	if err := json.Unmarshal(frozenPayload(t, response), &result); err != nil {
		t.Fatal(err)
	}
	projected, _ = result.Value.(map[string]any)
	if projected["interpolated"] != "x-ok-b" || projected["sum"] != float64(42) {
		t.Fatalf("known: %s", response)
	}
	// A value the constraint on the computed value refuses.
	if response := specialize([]scalarFill{fill("a", `"nope"`), fill("b", `"b"`), fill("n", "41")}, nil); !strings.Contains(response, `"error"`) {
		t.Fatalf("accepted a violated constraint: %s", response)
	}
	// A wrong or missing source is refused.
	if response := specialize(nil, []deferredValue{{Path: path("sum"), Target: source("a")}}); !strings.Contains(response, `"error"`) {
		t.Fatalf("accepted the wrong source: %s", response)
	}
	if response := specialize(nil, references[1:]); !strings.Contains(response, `"error"`) {
		t.Fatalf("accepted a source left out: %s", response)
	}
}
