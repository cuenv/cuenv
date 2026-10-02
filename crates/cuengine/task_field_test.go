package main

import (
	"strings"
	"testing"
)

func TestExportsField_FollowsTheProjectionAndTheField(t *testing.T) {
	projected := func(paths ...string) []projectionPath {
		t.Helper()
		parsed, err := parseProjectionPaths("exportPaths", paths)
		if err != nil {
			t.Fatalf("parse %v: %v", paths, err)
		}
		return parsed
	}
	cases := []struct {
		name     string
		paths    []projectionPath
		field    string
		expected bool
	}{
		{"whole instance", nil, "pipeline", true},
		{"projected field", projected("pipeline.build", "name"), "pipeline", true},
		{"other fields only", projected("name", "config.database"), "pipeline", false},
		{"a longer name is not the field", projected("pipelines"), "pipeline", false},
		{"disabled injection", nil, "", false},
		{"disabled injection with a projection", projected("name"), "", false},
	}
	for _, testCase := range cases {
		if got := exportsField(testCase.paths, testCase.field); got != testCase.expected {
			t.Errorf("%s: expected %v, got %v", testCase.name, testCase.expected, got)
		}
	}
}

func TestTaskFillPath_TargetsTheCallerSuppliedField(t *testing.T) {
	path, ok := taskFillPath("pipeline", "release-check[0].verify")
	if !ok {
		t.Fatal("expected a fill path")
	}
	if got := path.String(); !strings.HasPrefix(got, "pipeline.") {
		t.Errorf("expected a path below pipeline, got %s", got)
	}
	if _, ok := taskFillPath("pipeline", "release-check[x]"); ok {
		t.Error("a malformed index must not produce a path")
	}
}
