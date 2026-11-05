package main

import (
	"strings"
	"testing"
)

func importingModule(t *testing.T, env string) string {
	t.Helper()
	return writeCueModule(t, map[string]string{
		"schema/schema.cue": "package schema\n\n#Known: {value?: string}\nconstant: 1\n",
		"env.cue":           "package cuenv\n\nimport s \"" + testModulePath + "/schema\"\n\n" + env + "\n",
	})
}

func TestUndefinedImportedDefinitionFailsEvaluation(t *testing.T) {
	root := importingModule(t, `runtime: s.#Missing & {image: "alpine"}`)
	text := evaluation{moduleRoot: root, packageName: packageNamed("cuenv")}.failureText(t)
	if !strings.Contains(text, "#Missing") || !strings.Contains(text, "undefined in package") {
		t.Fatalf("failure does not name the undefined definition: %s", text)
	}
}

func TestUndefinedImportedFieldFailsEvaluation(t *testing.T) {
	root := importingModule(t, `value: s.missing`)
	text := evaluation{moduleRoot: root, packageName: packageNamed("cuenv")}.failureText(t)
	if !strings.Contains(text, "missing") {
		t.Fatalf("failure does not name the undefined field: %s", text)
	}
}

func TestDeclaredImportedMembersEvaluate(t *testing.T) {
	root := importingModule(t, "known: s.#Known & {value: \"x\"}\nconstant: s.constant")
	instance := evaluation{moduleRoot: root, packageName: packageNamed("cuenv")}.instance(t, ".")
	if instance["constant"] != float64(1) {
		t.Fatalf("unexpected instance: %v", instance)
	}
}

func TestShadowedImportNameIsNotChecked(t *testing.T) {
	root := importingModule(t, "known: s.#Known & {}\nnested: {\n\ts: {other: 1}\n\tvalue: s.other\n}")
	instance := evaluation{moduleRoot: root, packageName: packageNamed("cuenv")}.instance(t, ".")
	if _, ok := instance["nested"]; !ok {
		t.Fatalf("unexpected instance: %v", instance)
	}
}
