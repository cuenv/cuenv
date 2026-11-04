package main

import (
	"encoding/json"
	"strings"
	"testing"

	"cuelang.org/go/cue"
)

func TestFrozenNestedControlsRetainIterationSourceIdentity(t *testing.T) {
	for _, test := range []struct {
		name, control string
		valid         bool
	}{
		{"absent nested error", `for _, resource in resources {if (resource.outputs.flag & false) {_error: error("bad")}}`, false},
		{"absent nested field", `for _, resource in resources {if (resource.outputs.flag & false) {_error: true}}`, false},
		{"extra nesting", `for _, resource in resources {if true {if (resource.outputs.flag & false) {_error: true}}}`, false},
		{"embedded nested control", `for _, resource in resources {{if (resource.outputs.flag & false) {_error: true}}}`, false},
		{"parenthesized embedded control", `for _, resource in resources {({if (resource.outputs.flag & false) {_error: true}})}`, false},
		{"parenthesized hidden field control", `for _, resource in resources {_check: ({if (resource.outputs.flag & false) {_error: error("bad")}})}`, false},
		{"conjoined hidden field control", `for _, resource in resources {_check: {} & {if (resource.outputs.flag & false) {_error: true}}}`, false},
		{"nested parenthesized hidden control", `for _, resource in resources {_check: ({nested: ({if (resource.outputs.flag & false) {_error: true}})})}`, false},
		{"ordinary parenthesized hidden control", `for _, resource in resources {_check: ({if resource.type == "fake" {_ok: true}})}`, true},
		{"hidden list control", `for _, resource in resources {_check: [{if (resource.outputs.flag & false) {_error: true}}]}`, false},
		{"hidden builtin control", `for _, resource in resources {_check: close({if (resource.outputs.flag & false) {_error: true}})}`, false},
		{"multiply wrapped hidden control", `for _, resource in resources {_check: (({} & ({if (resource.outputs.flag & false) {_error: true}})))}`, false},
		{"ordinary conjoined hidden control", `for _, resource in resources {_check: {} & {if resource.type == "fake" {_ok: true}}}`, true},
		{"ordinary hidden builtin control", `for _, resource in resources {_check: close({if resource.type == "fake" {_ok: true}})}`, true},
		{"ordinary hidden list control", `for _, resource in resources {_check: [{if resource.type == "fake" {_ok: true}}]}`, true},
		{"ordinary wrapped hidden payload", `for _, resource in resources {_check: ({kind: resource.type})}`, true},
		{"ordinary conjoined lexical let", `for _, resource in resources {let item = resource
_check: {} & {if item.type == "fake" {_ok: true}}}`, true},
		{"conjoined lexical let output control", `for _, resource in resources {let item = resource
_check: {} & {if (item.outputs.flag & false) {_error: true}}}`, false},
		{"nested iteration ordinary conjunction", `for _, resource in resources {for _, item in resources {_check: {} & {if item.type == "fake" {_ok: true}}}}`, true},
		{"nested iteration conjoined output control", `for _, resource in resources {for _, item in resources {_check: {} & {if (item.outputs.flag & false) {_error: true}}}}`, false},
		{"wrapped nested lexical let ordinary control", `for _, resource in resources {_check: ({} & {let item = resource
inner: ({} & {if item.type == "fake" {_ok: true}})})}`, true},
		{"wrapped nested lexical let output control", `for _, resource in resources {_check: ({} & {let item = resource
inner: ({} & {if (item.outputs.flag & false) {_error: true}})})}`, false},
		{"literal bound list ordinary control", `for _, resource in resources {for _, item in [resource] {_check: {} & {if item.type == "fake" {_ok: true}}}}`, true},
		{"literal bound list output control", `for _, resource in resources {for _, item in [resource] {_check: {} & {if (item.outputs.flag & false) {_error: true}}}}`, false},
		{"wrapped literal bound list ordinary control", `for _, resource in resources {for _, item in ([resource]) {_check: ({if item.type == "fake" {_ok: true}})}}`, true},
		{"wrapped literal bound list output control", `for _, resource in resources {for _, item in ([resource]) {_check: ({if (item.outputs.flag & false) {_error: true}})}}`, false},
		{"nested literal bound list ordinary control", `for _, resource in resources {for _, item in [resource] {for _, nested in [item] {_check: {} & {if nested.type == "fake" {_ok: true}}}}}`, true},
		{"nested literal bound list output control", `for _, resource in resources {for _, item in [resource] {for _, nested in [item] {_check: {} & {if (nested.outputs.flag & false) {_error: true}}}}}`, false},
		{"nested lexical shadow retains outer source", `for _, resource in resources {let item = resource
_check: {first: ({let item = {outputs: flag: false}
if item.outputs.flag == false {_ok: true}})
second: ({if (item.outputs.flag & false) {_error: true}})}}`, false},
		{"ordinary field shadows iteration binding", `for _, resource in resources {_check: {resource: {outputs: flag: false}
if resource.outputs.flag == false {_ok: true}}}`, true},
		{"field shadows ordinary iteration alias with source", `for _, item in [{outputs: flag: false}] {_check: {item: resources.first
if (item.outputs.flag & false) {_error: true}}}`, false},
		{"fallback lexical let ordinary control", `for _, resource in resources {if false {_hidden: true} else {let item = resource
_check: {} & {if item.type == "fake" {_ok: true}}}}`, true},
		{"fallback lexical let output control", `for _, resource in resources {if false {_hidden: true} else {let item = resource
_check: {} & {if (item.outputs.flag & false) {_error: true}}}}`, false},
		{"iteration let clause ordinary control", `for _, resource in resources let item = resource {_check: {} & {if item.type == "fake" {_ok: true}}}`, true},
		{"iteration let clause output control", `for _, resource in resources let item = resource {_check: {} & {if (item.outputs.flag & false) {_error: true}}}`, false},
		{"constrained container presence", `if (resources.first & {outputs: flag: false}) != _|_ {_error: true}`, false},
		{"constrained container length", `if len(resources.first & {outputs: flag: false}) > 0 {_error: true}`, false},
		{"bound alias", `for _, resource in resources {let item = resource
if (item.outputs.flag & false) {_error: true}}`, false},
		{"quoted bound selector", `for _, resource in resources {if (resource["outputs"]["flag"] & false) {_error: true}}`, false},
		{"ordinary bound fields", `for _, resource in resources {if resource.type == "fake" {_check: true}}`, true},
		{"ordinary field names", `for _, resource in resources {let fields = [for field, _ in resource {field}]
if len(fields) > 0 {_check: true}}`, true},
		{"ordinary container length", `if len(resources) > 0 {_check: true}`, true},
	} {
		t.Run(test.name, func(t *testing.T) {
			check := func(config, outside string, want bool) {
				t.Helper()
				files := map[string]string{"env.cue": "package app\nresources: first: {type: \"fake\", outputs: flag: bool}\nconfig: " + config + "\n" + outside + "\n"}
				if strings.Contains(test.name, "fallback") {
					files["cue.mod/module.cue"] = "module: \"example.com/bridge@v0\"\nlanguage: version: \"v0.16.0\"\n"
					files["env.cue"] = "@experiment(try)\n" + files["env.cue"]
				}
				_, snapshot := freezeTestModule(t, files)
				sources := []valuePath{frozenPath("resources.first.outputs.flag")}
				response := specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config"), SourcePaths: &sources}))
				if strings.Contains(response, `"error"`) == want || strings.Contains(response, `"PANIC_RECOVER"`) {
					t.Fatalf("config %s, want accepted=%t: %s", config, want, response)
				}
			}
			// Outside of the configuration nothing reads it, and real values are verified.
			check(`{value: "literal"}`, test.control, true)
			// Fields of the configuration it decides are exported: those are refused.
			visible := strings.NewReplacer("_error", "visibleError", "_check", "visibleCheck", "_ok", "visibleOk").Replace(test.control)
			check(`{value: "literal", `+visible+`}`, "", test.valid)
		})
	}
}

// An invalid value is an error wherever it is, read or not.
func TestFrozenErroneousShadowFieldRejects(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
resources: first: {type: "fake", outputs: flag: bool}
config: value: "literal"
for _, resource in resources {_check: {resource: {outputs: flag: error("invalid")}
if resource.outputs.flag {_error: true}}}
`})
	sources := []valuePath{frozenPath("resources.first.outputs.flag")}
	response := specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config"), SourcePaths: &sources}))
	if !strings.Contains(response, `"error"`) {
		t.Fatalf("accepted an invalid value: %s", response)
	}
}

// A check the configuration does not read cannot change it. Real values are verified
// against the whole instance, so a violated check stops the run instead.
func TestFrozenChecksOutsideTheConfigurationAreAccepted(t *testing.T) {
	for _, test := range []struct{ name, declaration, source string }{
		{"unrelated predicate", `ignored: {source: id: string, check: source.id & !="bad"}`, "ignored.source.id"},
		{"root hidden check", "ignored: source: id: string\n_check: ignored.source.id & !=\"bad\"", "ignored.source.id"},
		{"root public check", "ignored: source: id: string\ncheck: ignored.source.id & !=\"bad\"", "ignored.source.id"},
		{"check elsewhere constrains a source", "source: id: string\nignored: check: source.id & !=\"bad\"", "source.id"},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\n" + test.declaration + "\nconfig: value: \"literal\"\n"})
			sources := []valuePath{frozenPath(test.source)}
			for operation, response := range map[string]string{
				"inspection":     evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}, SourcePaths: &sources})),
				"validation":     validateFrozenResponse(snapshot, frozenJSON(t, frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("config")}, SourcePaths: &sources})),
				"specialization": specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config"), SourcePaths: &sources})),
			} {
				if strings.Contains(response, `"error"`) {
					t.Fatalf("%s: %s", operation, response)
				}
			}
			verify := func(value string) string {
				return verifyFrozenResponse(snapshot, frozenJSON(t, frozenVerificationOptions{
					Fills:   []scalarFill{{Path: sources[0], Value: json.RawMessage(value)}},
					Roots:   []valuePath{frozenPath("config")},
					Mutable: sources,
				}))
			}
			if response := verify(`"good"`); strings.Contains(response, `"error"`) {
				t.Fatalf("verification refused an acceptable value: %s", response)
			}
			if response := verify(`"bad"`); !strings.Contains(response, `"error"`) {
				t.Fatalf("verification accepted a value violating the check: %s", response)
			}
		})
	}
}

func TestFrozenBootstrapScopeDoesNotHideVisibleReferences(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nignored: source: id: string\n_check: ignored.source.id & !=\"bad\"\nconfig: id: ignored.source.id\n"})
	empty := []valuePath{}
	response := evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}, SourcePaths: &empty}))
	var evaluation frozenEvaluation
	if err := json.Unmarshal(frozenPayload(t, response), &evaluation); err != nil {
		t.Fatal(err)
	}
	if len(evaluation.References) != 1 {
		t.Fatalf("bootstrap or ignored scope hid a selected reference: %s", response)
	}
	target, err := evaluation.References[0].Target.cuePath()
	if err != nil || target.String() != "ignored.source.id" {
		t.Fatalf("selected reference target changed: %s", response)
	}
}

// _alias in the schema and in the package are different fields. The reference goes
// through the schema's, and the constraint on the other one is still verified.
func TestFrozenImportedHiddenAliasIdentity(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{
		"schema/types.cue": "package schema\n#Root: {source: id: string, _alias: source.id, consumers: {bound: id: _alias, ...}}\n",
		"env.cue":          "package app\nimport \"example.com/bridge/schema\"\nschema.#Root\nsource: id: string\n_alias: \"local\"\nconsumers: independent: value: _alias\n",
	})
	sources := []valuePath{frozenPath("source.id")}
	response := evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{SourcePaths: &sources, ReferencePaths: []valuePath{frozenPath("consumers")}}))
	var evaluation frozenEvaluation
	if err := json.Unmarshal(frozenPayload(t, response), &evaluation); err != nil {
		t.Fatal(err)
	}
	if len(evaluation.References) != 1 || !evaluation.References[0].Supported {
		t.Fatalf("hidden fields of different packages were conflated: %s", response)
	}
	path, _ := evaluation.References[0].Path.cuePath()
	if path.String() != "consumers.bound.id" {
		t.Fatalf("reference at %s", path)
	}
}

func TestFrozenChecksDoNotAffectConsumersThatDoNotReadThem(t *testing.T) {
	for _, test := range []struct{ name, declaration, bound string }{
		{"bare alias", `_alias: source.id`, `id: _alias`},
		{"bare object alias", `_alias: {id: source.id}`, `_alias`},
		{"bare list alias", `_alias: [source.id]`, `ids: _alias`},
		{"constraint propagated by alias", `_alias: source.id & !="bad"`, `id: _alias`},
		{"independent hidden check", `_check: source.id & !="bad"`, `id: source.id`},
		{"independent public check", `check: source.id & !="bad"`, `id: source.id`},
		{"independent definition check", `#Check: source.id & !="bad"`, `id: source.id`},
		{"same text is not same expression", `_check: source.id & !="bad"`, `id: source.id & !="bad"`},
		{"unused constrained alias", `_alias: source.id & !="bad"`, `id: source.id`},
		{"extra predicate on tracked alias", "_alias: source.id & !=\"bad\"\n_check: _alias & \"ok\"", `id: _alias`},
		{"hidden list predicate", `_check: [source.id & !="bad"]`, `id: source.id`},
		{"hidden object predicate", `_check: {id: source.id & !="bad"}`, `id: source.id`},
		{"hidden output condition", `if (source.id & "ok") == "ok" {_check: true}`, `id: source.id`},
		{"no visible use", `_check: source.id & !="bad"`, `value: "literal"`},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: id: string\n" + test.declaration + "\nconsumers: {bound: {" + test.bound + "}, independent: value: \"literal\"}\n"})
			sources := []valuePath{frozenPath("source.id")}
			regions := []valuePath{frozenPath("consumers")}
			response := evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: regions, SourcePaths: &sources}))
			var evaluation frozenEvaluation
			if err := json.Unmarshal(frozenPayload(t, response), &evaluation); err != nil {
				t.Fatal(err)
			}
			for _, reference := range evaluation.References {
				path, _ := reference.Path.cuePath()
				if !reference.Supported || !pathWithin(path, cue.ParsePath("consumers.bound")) {
					t.Fatalf("%s: %s", path, response)
				}
			}
			frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("consumers.independent"), SourcePaths: &sources})))
		})
	}
}

func TestFrozenLiteralIterationPreservesPartialOrdinaryContainers(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
source: string
resources: first: {type: "fake", ordinary: string}
config: value: "literal"
for _, resource in resources {
    for _, item in ([resource]) {
        _check: {} & {if item.type == "fake" {_ok: true}}
    }
}`})
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		t.Fatal(err)
	}
	value, err := graph.build()
	if err != nil || value.Err() != nil {
		t.Fatalf("invalid original CUE: %v %v", err, value.Err())
	}
	if err := value.LookupPath(cue.ParsePath("config")).Validate(cue.Concrete(true)); err != nil {
		t.Fatal(err)
	}
	sources := []valuePath{frozenPath("source")}
	for operation, response := range map[string]string{
		"inspection":     evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}, SourcePaths: &sources})),
		"specialization": specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config"), SourcePaths: &sources})),
	} {
		if strings.Contains(response, `"error"`) || !strings.Contains(response, `"ok"`) {
			t.Fatalf("%s rejected ordinary partial CUE: %s", operation, response)
		}
	}
}
