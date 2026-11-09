package main

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"

	"cuelang.org/go/cue/cuecontext"
)

const contextTestSchema = `
#Action: "plan" | "apply" | "destroy"
#Context: {
	command:        "task" | "exec" | "infrastructure" | "shell"
	task:           string | null
	exec:           string | null
	infrastructure: #Action | null
	ci: null | {provider: string, event: string | null, ref: string | null, actor: string | null}
	user: {name: string | null, email: string | null}
	host: {os: string, arch: string, name: string | null}
	vcs: {branch: string | null}
}
contexts: [string]: when: #Context

`

func exportContexts(t *testing.T, source string) (map[string]interface{}, error) {
	t.Helper()
	value := cuecontext.New().CompileString(contextTestSchema + source)
	if err := value.Err(); err != nil {
		t.Fatalf("compile: %v", err)
	}
	exported, ok := buildValueClean(value).(map[string]interface{})
	if !ok {
		t.Fatal("expected a struct")
	}
	if err := attachContextMatchers(value, exported); err != nil {
		return nil, err
	}
	// Round-trip so the assertions see exactly what the caller receives.
	encoded, err := json.Marshal(exported["contexts"])
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	var contexts map[string]interface{}
	if err := json.Unmarshal(encoded, &contexts); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	return contexts, nil
}

func contextFieldMatcher(t *testing.T, contexts map[string]interface{}, name string, path ...string) interface{} {
	t.Helper()
	var current interface{} = contexts[name].(map[string]interface{})["when"]
	for _, label := range path {
		fields, ok := current.(map[string]interface{})["fields"].(map[string]interface{})
		if !ok {
			t.Fatalf("%s: no fields at %s in %v", name, label, current)
		}
		current = fields[label]
	}
	return current
}

func TestAttachContextMatchers_CompilesConstraints(t *testing.T) {
	contexts, err := exportContexts(t, `
contexts: {
	plan:        when: infrastructure: "plan"
	write:       when: infrastructure: "apply" | "destroy"
	deploy:      when: task:           =~"^deploy" & !~"dry$"
	pullRequest: when: ci: event: "pull_request"
	local:       when: ci: null
	notMain:     when: vcs: branch: !="main"
	localPlan:   when: local.when & plan.when
}
`)
	if err != nil {
		t.Fatal(err)
	}
	equals := func(value interface{}) map[string]interface{} {
		return map[string]interface{}{"equals": value}
	}
	cases := []struct {
		context  string
		path     []string
		expected interface{}
	}{
		{"plan", []string{"infrastructure"}, equals("plan")},
		{"pullRequest", []string{"ci", "event"}, equals("pull_request")},
		{"local", []string{"ci"}, equals(nil)},
		{"localPlan", []string{"ci"}, equals(nil)},
		{"localPlan", []string{"infrastructure"}, equals("plan")},
		{"plan", []string{"host", "os"}, map[string]interface{}{"kind": "string"}},
		{"plan", []string{"task"}, map[string]interface{}{"anyOf": []interface{}{map[string]interface{}{"kind": "string"}, equals(nil)}}},
	}
	for _, testCase := range cases {
		got := contextFieldMatcher(t, contexts, testCase.context, testCase.path...)
		if !reflect.DeepEqual(got, testCase.expected) {
			t.Errorf("%s %v: expected %v, got %v", testCase.context, testCase.path, testCase.expected, got)
		}
	}

	deploy, err := json.Marshal(contextFieldMatcher(t, contexts, "deploy", "task"))
	if err != nil {
		t.Fatal(err)
	}
	for _, expected := range []string{`{"matches":"^deploy"}`, `{"notMatches":"dry$"}`} {
		if !strings.Contains(string(deploy), expected) {
			t.Errorf("deploy.task: expected %s in %s", expected, deploy)
		}
	}
	// A disjunction keeps the schema's own constraint beside the author's.
	write, err := json.Marshal(contextFieldMatcher(t, contexts, "write", "infrastructure"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(write), `{"anyOf":[{"equals":"apply"},{"equals":"destroy"}]}`) {
		t.Errorf("write.infrastructure: expected the disjunction in %s", write)
	}
	notMain, err := json.Marshal(contextFieldMatcher(t, contexts, "notMain", "vcs", "branch"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(notMain), `{"notEquals":"main"}`) {
		t.Errorf("notMain.vcs.branch: expected notEquals in %s", notMain)
	}
	// An unconstrained ci still admits both a CI run and none.
	planCI, err := json.Marshal(contextFieldMatcher(t, contexts, "plan", "ci"))
	if err != nil {
		t.Fatal(err)
	}
	for _, expected := range []string{`{"equals":null}`, `"fields"`} {
		if !strings.Contains(string(planCI), expected) {
			t.Errorf("plan.ci: expected %s in %s", expected, planCI)
		}
	}
}

func TestAttachContextMatchers_ListsContextsThatCanHoldTogether(t *testing.T) {
	contexts, err := exportContexts(t, `
contexts: {
	plan:        when: infrastructure: "plan"
	write:       when: infrastructure: "apply" | "destroy"
	pullRequest: when: ci: event: "pull_request"
	local:       when: ci: null
}
`)
	if err != nil {
		t.Fatal(err)
	}
	expected := map[string][]interface{}{
		"plan":        {"local", "pullRequest"},
		"write":       {"local", "pullRequest"},
		"pullRequest": {"plan", "write"},
		"local":       {"plan", "write"},
	}
	for name, overlaps := range expected {
		got := contexts[name].(map[string]interface{})["overlaps"]
		if !reflect.DeepEqual(got, overlaps) {
			t.Errorf("%s: expected overlaps %v, got %v", name, overlaps, got)
		}
	}
}

func TestAttachContextMatchers_RejectsWhatItCannotMatch(t *testing.T) {
	cases := []struct {
		name     string
		source   string
		expected string
	}{
		{"a bound", `contexts: long: when: task: >"m"`, "cannot be matched"},
		{"an interpolation", `contexts: long: when: task: "deploy-\(contexts.long.when.host.os)"`, "cannot be matched"},
	}
	for _, testCase := range cases {
		_, err := exportContexts(t, testCase.source)
		if err == nil {
			t.Errorf("%s: expected an error", testCase.name)
			continue
		}
		if !strings.Contains(err.Error(), testCase.expected) || !strings.Contains(err.Error(), "contexts.long.when") {
			t.Errorf("%s: unexpected error %v", testCase.name, err)
		}
	}
}

func TestAttachContextMatchers_LeavesOtherExportsAlone(t *testing.T) {
	value := cuecontext.New().CompileString(`name: "project"`)
	exported := buildValueClean(value).(map[string]interface{})
	if err := attachContextMatchers(value, exported); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(exported, map[string]interface{}{"name": "project"}) {
		t.Errorf("unexpected export %v", exported)
	}
}
