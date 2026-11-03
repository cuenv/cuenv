package main

import (
	"encoding/json"
	"strings"
	"testing"
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
		{"constrained container presence", `if (resources.first & {outputs: flag: false}) != _|_ {_error: true}`, false},
		{"constrained container length", `if len(resources.first & {outputs: flag: false}) > 0 {_error: true}`, false},
		{"bound alias", `for _, resource in resources {let item = resource
if (item.outputs.flag & false) {_error: true}}`, false},
		{"quoted bound selector", `for _, resource in resources {if (resource["outputs"]["flag"] & false) {_error: true}}`, false},
		{"whole container condition", `if (resources.first & {outputs: flag: false}) == {type: "fake", outputs: flag: false} {_error: true}`, false},
		{"ordinary bound fields", `for _, resource in resources {if resource.type == "fake" {_check: true}}`, true},
		{"ordinary field names", `for _, resource in resources {let fields = [for field, _ in resource {field}]
if len(fields) > 0 {_check: true}}`, true},
		{"ordinary container length", `if len(resources) > 0 {_check: true}`, true},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nresources: first: {type: \"fake\", outputs: flag: bool}\nconfig: value: \"literal\"\n" + test.control + "\n"})
			sources := []valuePath{frozenPath("resources.first.outputs.flag")}
			for operation, response := range map[string]string{
				"inspection":     evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}, SourcePaths: &sources})),
				"specialization": specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config"), SourcePaths: &sources})),
			} {
				if strings.Contains(response, `"error"`) == test.valid || strings.Contains(response, `"PANIC_RECOVER"`) {
					if test.valid {
						var graph frozenGraph
						_ = json.Unmarshal([]byte(snapshot), &graph)
						value, _ := graph.build()
						t.Logf("fixture inspection cause: %v", validateUntrackedDependencies(dependencyInspection{instance: value, scope: frozenDependencyScope{SourcePaths: &sources}}))
					}
					t.Fatalf("%s valid=%t: %s", operation, test.valid, response)
				}
			}
		})
	}
}

func TestFrozenDependencyScopeIgnoredAndInvalidSources(t *testing.T) {
	for _, test := range []struct {
		name, declaration, source string
		valid                     bool
	}{
		{"unrelated ignored predicate", `ignored: {source: id: string, check: source.id & !="bad"}`, "ignored.source.id", true},
		{"root hidden check uses ignored source", "ignored: source: id: string\n_check: ignored.source.id & !=\"bad\"", "ignored.source.id", false},
		{"root public check uses ignored source", "ignored: source: id: string\ncheck: ignored.source.id & !=\"bad\"", "ignored.source.id", false},
		{"ignored check constrains selected source", "source: id: string\nignored: check: source.id & !=\"bad\"", "source.id", false},
		{"refined ignored source", "ignored: source: id: string & !=\"\"\n_check: ignored.source.id & !=\"bad\"", "ignored.source.id", false},
		{"refined alias source", "upstream: string\nignored: source: id: upstream & !=\"\"\n_check: ignored.source.id & !=\"bad\"", "ignored.source.id", false},
		{"invalid nested output attribute", "ignored: source: bad: nested: string\n_check: ignored.source.bad.nested & !=\"bad\"", "ignored.source.bad", false},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\n" + test.declaration + "\nconfig: value: \"literal\"\n"})
			sources := []valuePath{frozenPath(test.source)}
			ignored := []valuePath{frozenPath("ignored"), frozenPath("missing")}
			for operation, response := range map[string]string{
				"inspection":     evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}, SourcePaths: &sources, IgnoredPaths: ignored})),
				"validation":     validateFrozenResponse(snapshot, frozenJSON(t, frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("config")}, SourcePaths: &sources, IgnoredPaths: ignored})),
				"specialization": specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("config"), SourcePaths: &sources, IgnoredPaths: ignored})),
			} {
				if strings.Contains(response, `"error"`) == test.valid || strings.Contains(response, `"PANIC_RECOVER"`) {
					t.Fatalf("%s valid=%t: %s", operation, test.valid, response)
				}
			}
		})
	}
}

func TestFrozenBootstrapScopeDoesNotHideVisibleReferences(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nignored: source: id: string\n_check: ignored.source.id & !=\"bad\"\nconfig: id: ignored.source.id\n"})
	empty := []valuePath{}
	response := evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: []valuePath{frozenPath("config")}, SourcePaths: &empty, IgnoredPaths: []valuePath{frozenPath("ignored")}}))
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

func TestFrozenImportedHiddenAliasIdentity(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{
		"schema/types.cue": "package schema\n#Root: {source: id: string, _alias: source.id, consumers: {bound: id: _alias, ...}}\n",
		"env.cue":          "package app\nimport \"example.com/bridge/schema\"\nschema.#Root\nsource: id: string\n_alias: source.id & !=\"bad\"\nconsumers: independent: value: \"literal\"\n",
	})
	sources := []valuePath{frozenPath("source.id")}
	regions := []valuePath{frozenPath("consumers")}
	for operation, response := range map[string]string{
		"inspection":     evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{SourcePaths: &sources, ReferencePaths: regions})),
		"specialization": specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{SourcePaths: &sources, ReferencePaths: regions, Projection: frozenPath("consumers.independent")})),
	} {
		if !strings.Contains(response, `"error"`) {
			t.Fatalf("%s conflated hidden fields from different packages: %s", operation, response)
		}
	}
}

func TestFrozenIgnoredGroupsKeepDependencyBoundaries(t *testing.T) {
	for _, test := range []struct {
		name, check string
		valid       bool
	}{
		{"same configuration namespaces", `inactive: resources: check: inactive.data.source.id & !="bad"`, true},
		{"root predicate", `_check: inactive.data.source.id & !="bad"`, false},
		{"another ignored configuration", `other: check: inactive.data.source.id & !="bad"`, false},
		{"another selected configuration", `config: check: inactive.data.source.id & !="bad"`, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\ninactive: {resources: {}, data: source: id: string}\nconfig: value: \"literal\"\n" + test.check + "\n"})
			sources := []valuePath{frozenPath("inactive.data.source.id")}
			groups := [][]valuePath{{frozenPath("inactive.resources"), frozenPath("inactive.data")}}
			response := evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{SourcePaths: &sources, ReferencePaths: []valuePath{frozenPath("config.value")}, IgnoredPaths: []valuePath{frozenPath("other")}, IgnoredGroups: groups}))
			if strings.Contains(response, `"error"`) == test.valid {
				t.Fatalf("valid=%t: %s", test.valid, response)
			}
		})
	}
}

func TestFrozenGlobalPredicatesRequireExactTrackedExpressions(t *testing.T) {
	for _, test := range []struct {
		name, declaration, bound string
		valid                    bool
	}{
		{"bare alias", `_alias: source.id`, `id: _alias`, true},
		{"bare object alias", `_alias: {id: source.id}`, `_alias`, true},
		{"bare list alias", `_alias: [source.id]`, `ids: _alias`, true},
		{"constraint propagated by alias", `_alias: source.id & !="bad"`, `id: _alias`, true},
		{"independent hidden check", `_check: source.id & !="bad"`, `id: source.id`, false},
		{"independent public check", `check: source.id & !="bad"`, `id: source.id`, false},
		{"independent definition check", `#Check: source.id & !="bad"`, `id: source.id`, false},
		{"same text is not same expression", `_check: source.id & !="bad"`, `id: source.id & !="bad"`, false},
		{"unused constrained alias", `_alias: source.id & !="bad"`, `id: source.id`, false},
		{"extra predicate on tracked alias", "_alias: source.id & !=\"bad\"\n_check: _alias & \"ok\"", `id: _alias`, false},
		{"hidden list predicate", `_check: [source.id & !="bad"]`, `id: source.id`, false},
		{"hidden object predicate", `_check: {id: source.id & !="bad"}`, `id: source.id`, false},
		{"hidden output condition", `if (source.id & "ok") == "ok" {_check: true}`, `id: source.id`, false},
		{"no visible use", `_check: source.id & !="bad"`, `value: "literal"`, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: id: string\n" + test.declaration + "\nconsumers: {bound: {" + test.bound + "}, independent: {value: \"literal\"}}\n"})
			sources := []valuePath{frozenPath("source.id")}
			regions := []valuePath{frozenPath("consumers")}
			inspection := evaluateFrozenResponse(snapshot, frozenJSON(t, frozenEvaluationOptions{ReferencePaths: regions, SourcePaths: &sources}))
			independent := specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: frozenPath("consumers.independent"), ReferencePaths: regions, SourcePaths: &sources}))
			if test.valid {
				frozenPayload(t, inspection)
				frozenPayload(t, independent)
				return
			}
			validation := validateFrozenResponse(snapshot, frozenJSON(t, frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("consumers.independent")}, SourcePaths: &sources}))
			for operation, response := range map[string]string{"inspection": inspection, "validation": validation, "independent consumer": independent} {
				if !strings.Contains(response, `"error"`) || strings.Contains(response, `"PANIC_RECOVER"`) {
					t.Fatalf("%s accepted independent predicate or panicked: %s", operation, response)
				}
			}
		})
	}
}
