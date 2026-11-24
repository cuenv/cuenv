package main

import (
	"encoding/json"
	"testing"
)

// A reference through a let binding, a hidden field or a label alias names
// the field it reaches, whatever the binding is called.
func TestModuleReferencesResolveAliasesToTheirField(t *testing.T) {
	root := writePackage(t, `
#Task: {command: string}
tasks: {
	build: #Task & {command: "build"}
	test: #Task & {command: "test"}
	lint: #Task & {command: "lint"}
}
let declared = tasks
_named: tasks
_chained: _named
tasks~T: _
pipelines: [...#Task]
pipelines: [declared.build, _named.test, T.lint, _chained.build]
`)
	options, err := json.Marshal(ModuleEvalOptions{
		WithMeta: true, WithReferences: true, PackageName: packageNamed(testPackageName),
	})
	if err != nil {
		t.Fatal(err)
	}
	response := decodeEnvelope(t, evaluateModuleResponse(root, "", string(options)))
	if response.Error != nil {
		t.Fatalf("evaluate project: %+v", response.Error)
	}
	var result ModuleResult
	if err := json.Unmarshal(response.Ok, &result); err != nil {
		t.Fatal(err)
	}
	for key, want := range map[string]string{
		"./pipelines[0]": "tasks.build",
		"./pipelines[1]": "tasks.test",
		"./pipelines[2]": "tasks.lint",
		"./pipelines[3]": "tasks.build",
	} {
		if got := result.Meta[key].Reference; got != want {
			t.Errorf("reference %s = %q, want %q", key, got, want)
		}
	}
}

// A list entry written as a definition unified with a struct is a value of
// its own, not a reference to the definition.
func TestModuleReferencesIgnoreDefinitionsUsedAsValues(t *testing.T) {
	root := writePackage(t, `
#Task: {command: string, name?: string}
tasks: build: #Task & {command: "build"}
pipelines: [...]
pipelines: [tasks.build, #Task & {command: "inline", name: "announce"}]
`)
	options, err := json.Marshal(ModuleEvalOptions{
		WithMeta: true, WithReferences: true, PackageName: packageNamed(testPackageName),
	})
	if err != nil {
		t.Fatal(err)
	}
	response := decodeEnvelope(t, evaluateModuleResponse(root, "", string(options)))
	if response.Error != nil {
		t.Fatalf("evaluate project: %+v", response.Error)
	}
	var result ModuleResult
	if err := json.Unmarshal(response.Ok, &result); err != nil {
		t.Fatal(err)
	}
	if got := result.Meta["./pipelines[0]"].Reference; got != "tasks.build" {
		t.Errorf("reference of a task = %q, want tasks.build", got)
	}
	if got := result.Meta["./pipelines[1]"].Reference; got != "" {
		t.Errorf("an inline value has reference %q, want none", got)
	}
}
