package main

import (
	"encoding/json"
	"os"
	"reflect"
	"strings"
	"sync"
	"testing"
)

func preparedFixture(t *testing.T, snapshot string) string {
	t.Helper()
	capability, err := prepareFrozen(snapshot, frozenJSON(t, preparationOptions{ReferencePaths: []valuePath{frozenPath("config")}, Projections: []valuePath{frozenPath("config")}}))
	if err != nil {
		t.Fatal(err)
	}
	return capability
}

func TestPreparedMatchesFreshSpecialization(t *testing.T) {
	directory, snapshot := frozenFixture(t)
	capability := preparedFixture(t, snapshot)
	if err := os.RemoveAll(directory); err != nil {
		t.Fatal(err)
	}
	cases := []specializationOptions{
		{Projection: frozenPath("config"), Deferred: fixtureDeferred()},
		{Projection: frozenPath("config"), Deferred: fixtureDeferred(), Fills: []scalarFill{{Path: frozenPath("source.id"), Value: json.RawMessage(`"ok"`)}}},
		{Projection: frozenPath("config"), Fills: []scalarFill{{Path: frozenPath("source.id"), Value: json.RawMessage(`"ok"`)}, {Path: frozenPath("source.flag"), Value: json.RawMessage(`true`)}, {Path: frozenPath("source.amount"), Value: json.RawMessage(`18446744073709551616.1234567890123456789`)}}},
		{Projection: frozenPath("config"), Deferred: fixtureDeferred(), Fills: []scalarFill{{Path: frozenPath("source.id"), Value: json.RawMessage(`"x"`)}}},
		{Projection: frozenPath("config")},
	}
	for index, options := range cases {
		var ordinary, prepared testEnvelope
		if err := json.Unmarshal([]byte(specializeFrozenResponse(snapshot, frozenJSON(t, options))), &ordinary); err != nil {
			t.Fatal(err)
		}
		if err := json.Unmarshal([]byte(specializePreparedResponse(snapshot, capability, frozenJSON(t, options))), &prepared); err != nil {
			t.Fatal(err)
		}
		if (ordinary.Error == nil) != (prepared.Error == nil) || !reflect.DeepEqual(ordinary.Ok, prepared.Ok) {
			t.Fatalf("case %d differs", index)
		}
	}
	// Repeated partial and empty fills must not inherit previous results.
	for iteration := 0; iteration < 3; iteration++ {
		result, err := specializePrepared(snapshot, capability, frozenJSON(t, cases[0]))
		if err != nil || len(result.Unresolved) != len(fixtureDeferred()) {
			t.Fatalf("unknown provenance changed: %v", err)
		}
	}
}

func TestPreparedAuthorityIsExactAndProjectionLocal(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
source: {a: string, b: string}
consumers: {a: {id: source.a & "ok"}, b: {id: source.b}}
`})
	options := preparationOptions{ReferencePaths: []valuePath{frozenPath("consumers")}, Projections: []valuePath{frozenPath("consumers.a"), frozenPath("consumers.b")}}
	capability, err := prepareFrozen(snapshot, frozenJSON(t, options))
	if err != nil {
		t.Fatal(err)
	}
	valid := preparedSpecializationOptions{Projection: frozenPath("consumers.a"), Deferred: []deferredValue{{Path: frozenPath("consumers.a.id"), Target: frozenPath("source.a")}}}
	result, err := specializePrepared(snapshot, capability, frozenJSON(t, valid))
	if err != nil || len(result.Unresolved) != 1 {
		t.Fatalf("narrowed reference lost provenance: %v", err)
	}
	for _, request := range []preparedSpecializationOptions{
		{Projection: frozenPath("consumers.a.id")},
		{Projection: frozenPath("consumers")},
		{Projection: valid.Projection, Fills: []scalarFill{{Path: frozenPath("source.b"), Value: json.RawMessage(`"ok"`)}}, Deferred: valid.Deferred},
		{Projection: valid.Projection, Deferred: []deferredValue{{Path: frozenPath("consumers.a.id"), Target: frozenPath("source.b")}}},
		{Projection: valid.Projection, Deferred: append(valid.Deferred, deferredValue{Path: frozenPath("consumers.b.id"), Target: frozenPath("source.b")})},
	} {
		if _, err := specializePrepared(snapshot, capability, frozenJSON(t, request)); err == nil {
			t.Fatal("accepted authority outside exact projection")
		}
	}
	options.ReferencePaths = []valuePath{frozenPath("consumers.a")}
	if _, err := prepareFrozen(snapshot, frozenJSON(t, options)); err == nil {
		t.Fatal("accepted projection outside checked roots")
	}
	options.ReferencePaths = []valuePath{frozenPath("consumers")}
	empty := []valuePath{}
	options.SourcePaths = &empty
	if _, err := prepareFrozen(snapshot, frozenJSON(t, options)); err == nil {
		t.Fatal("bootstrap source scope authorized runtime references")
	}
}

func TestPreparedCapabilityRejectsTamperingAndChangedSnapshot(t *testing.T) {
	_, snapshot := frozenFixture(t)
	capability := preparedFixture(t, snapshot)
	request := frozenJSON(t, preparedSpecializationOptions{Projection: frozenPath("config"), Deferred: fixtureDeferred()})
	var token preparedCapability
	if err := json.Unmarshal([]byte(capability), &token); err != nil {
		t.Fatal(err)
	}
	var payload map[string]interface{}
	if err := json.Unmarshal(token.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{"version", "snapshotDigest", "scope", "projections"} {
		copy := make(map[string]interface{})
		for key, value := range payload {
			copy[key] = value
		}
		copy[field] = nil
		modified := token
		modified.Payload = json.RawMessage(frozenJSON(t, copy))
		if _, err := specializePrepared(snapshot, frozenJSON(t, modified), request); err == nil {
			t.Fatalf("accepted modified %s", field)
		}
	}
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		t.Fatal(err)
	}
	for index := range graph.Nodes {
		graph.Nodes[index].Files[0].Source += "\n// changed\n"
		if _, err := specializePrepared(frozenJSON(t, graph), capability, request); err == nil {
			t.Fatal("accepted changed source/import snapshot")
		}
	}
	for _, invalid := range []string{"{}", "null", capability[:len(capability)/2]} {
		response := specializePreparedResponse(snapshot, invalid, request)
		if strings.Contains(response, "source.id") || strings.Contains(response, "shared") {
			t.Fatal("error exposed source data")
		}
	}
}

func TestPreparedStillValidatesWholeInstance(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
source: id: string
_alias: source.id & "ok"
consumers: {first: id: _alias, second: id: source.id}
`})
	capability, err := prepareFrozen(snapshot, frozenJSON(t, preparationOptions{ReferencePaths: []valuePath{frozenPath("consumers")}, Projections: []valuePath{frozenPath("consumers.second")}}))
	if err != nil {
		t.Fatal(err)
	}
	for _, value := range []string{`"x"`, `true`, `13.25`} {
		options := preparedSpecializationOptions{Projection: frozenPath("consumers.second"), Fills: []scalarFill{{Path: frozenPath("source.id"), Value: json.RawMessage(value)}}}
		if _, err := specializePrepared(snapshot, capability, frozenJSON(t, options)); err == nil {
			t.Fatal("failed to validate global constraints")
		}
	}
}

func TestPreparedRejectsStructuralDependencies(t *testing.T) {
	for _, declaration := range []string{
		`config: {if source.flag {other: "yes"}}`,
		`config: {for _, item in resources {if (item.outputs.flag & false) {other: true}}}`,
		`_decided: {if source.flag {other: "yes"}}
config: read: _decided`,
		`config: other: *source.id | "fallback"`,
		`config: {("\(source.id)"): "value"}`,
	} {
		t.Run(declaration, func(t *testing.T) {
			_, snapshot := freezeTestModule(t, map[string]string{"env.cue": "package app\nsource: {id:string, flag:bool}\nresources: one: outputs: flag: bool\nconfig: id: source.id\n" + declaration + "\n"})
			if _, err := prepareFrozen(snapshot, frozenJSON(t, preparationOptions{ReferencePaths: []valuePath{frozenPath("config")}, Projections: []valuePath{frozenPath("config")}})); err == nil {
				t.Fatal("prepared unsupported dependency")
			}
		})
	}
}

func TestPreparedConcurrentFreshContexts(t *testing.T) {
	_, snapshot := frozenFixture(t)
	capability := preparedFixture(t, snapshot)
	request := frozenJSON(t, preparedSpecializationOptions{Projection: frozenPath("config"), Deferred: fixtureDeferred()})
	var workers sync.WaitGroup
	for worker := 0; worker < 8; worker++ {
		workers.Add(1)
		go func() {
			defer workers.Done()
			for iteration := 0; iteration < 10; iteration++ {
				result, err := specializePrepared(snapshot, capability, request)
				if err != nil || len(result.Unresolved) != len(fixtureDeferred()) {
					t.Errorf("concurrent specialization failed: %v", err)
					return
				}
			}
		}()
	}
	workers.Wait()
}
