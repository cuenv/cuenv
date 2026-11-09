package main

import (
	"encoding/json"
	"testing"

	"cuelang.org/go/cue"
)

func TestReferencesThroughReusableDefinition(t *testing.T) {
	instance := compileAnalyzed(`package app
#Task: {
	command: string
	args?: [...string]
	dependsOn?: [...#Task]
}
#PipelineTask: #Task | {type: "matrix", task: #Task}
#Project: {
	tasks: [string]: #Task
	ci: pipelines: [string]: tasks: [...#PipelineTask]
}
#Project
#Service: {
	tasks: {
		migrate: #Task & { command: "echo", args: ["migrate"] }
		deploy: #Task & { command: "echo", args: ["deploy"], dependsOn: [migrate] }
	}
	_tasks: tasks
	ci: pipelines: default: tasks: [_tasks.deploy]
}
_svc: #Service
tasks: _svc.tasks
ci: _svc.ci
`)
	if err := instance.Err(); err != nil {
		t.Fatal(err)
	}
	refs := make(map[string]string)
	extractReferencesFromValue(instance.Value, ".", "", refs)
	for key, want := range map[string]string{
		"./tasks.deploy.dependsOn[0]":                  "tasks.migrate",
		"./ci.pipelines.default.tasks[0]":              "#Service._tasks.deploy",
		"./ci.pipelines.default.tasks[0].dependsOn[0]": "#Service.tasks.migrate",
	} {
		if refs[key] != want {
			t.Errorf("reference %s = %q, want %q", key, refs[key], want)
		}
	}
}

const ambiguousConjunction = `
#Shape: {value: string}
values: {
	first: #Shape & {value: "same"}
	second: #Shape & {value: "same"}
}
ambiguous: values.first & values.second & #Shape
schemaOnly: #Shape & {value: "same"}
`

func TestConstrainedReferenceRejectsAmbiguousValues(t *testing.T) {
	instance := compileAnalyzed("package app\n" + ambiguousConjunction)
	if err := instance.Err(); err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{"ambiguous", "schemaOnly"} {
		if reference := safeReferencePath(instance.LookupPath(cue.ParsePath(field))); reference != "" {
			t.Errorf("%s unexpectedly identifies %q as its reference", field, reference)
		}
	}
}

func TestModuleReferenceMetadataDoesNotGuessAmbiguousConjunctions(t *testing.T) {
	root := writePackage(t, ambiguousConjunction)
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
	if reference := result.Meta["./ambiguous"].Reference; reference != "" {
		t.Errorf("ambiguous conjunction unexpectedly identifies %q", reference)
	}
}
