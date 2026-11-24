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
