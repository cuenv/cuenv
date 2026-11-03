package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"sync"
	"testing"
)

func TestPreparedSelectedScopeAndComponentAuthentication(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
environments: {
 dev: {source: id: string, config: id: source.id}
 prod: {source: id: string, config: id: source.id & "prod"}
}
inactive: {
 resources: producer: outputs: id: string
 data: consumer: id: inactive.resources.producer.outputs.id & "inactive"
}
`})
	sources := []valuePath{frozenPath("environments.dev.source.id"), frozenPath("environments.prod.source.id"), frozenPath("inactive.resources.producer.outputs.id")}
	options := preparationOptions{
		ReferencePaths: []valuePath{frozenPath("environments.dev.config")},
		SourcePaths:    &sources,
		IgnoredPaths:   []valuePath{frozenPath("environments.prod")},
		IgnoredGroups:  [][]valuePath{{frozenPath("inactive.resources"), frozenPath("inactive.data")}},
		Projections:    []valuePath{frozenPath("environments.dev.config")},
	}
	capability, err := prepareFrozen(snapshot, frozenJSON(t, options))
	if err != nil {
		t.Fatal(err)
	}
	request := frozenJSON(t, preparedSpecializationOptions{Projection: options.Projections[0], Fills: []scalarFill{{Path: sources[0], Value: json.RawMessage(`"dev"`)}}})
	if _, err := specializePrepared(snapshot, capability, request); err != nil {
		t.Fatal(err)
	}
	mutations := map[string]func(*preparedProvenance){
		"reference roots":       func(proof *preparedProvenance) { proof.Scope.ReferencePaths = nil },
		"source nil":            func(proof *preparedProvenance) { proof.Scope.SourcePaths = nil },
		"source empty":          func(proof *preparedProvenance) { empty := []valuePath{}; proof.Scope.SourcePaths = &empty },
		"ignored roots":         func(proof *preparedProvenance) { proof.Scope.IgnoredPaths = nil },
		"ignored groups":        func(proof *preparedProvenance) { proof.Scope.IgnoredGroups = nil },
		"requested projections": func(proof *preparedProvenance) { proof.Scope.Projections = nil },
		"authorized projection": func(proof *preparedProvenance) { proof.Projections[0].Path = frozenPath("environments.prod.config") },
		"reference destination": func(proof *preparedProvenance) {
			proof.Projections[0].References[0].Path = frozenPath("environments.dev.config.other")
		},
		"reference target":  func(proof *preparedProvenance) { proof.Projections[0].References[0].Target = sources[1] },
		"reference type":    func(proof *preparedProvenance) { proof.Projections[0].References[0].PrimitiveType = "number" },
		"reference support": func(proof *preparedProvenance) { proof.Projections[0].References[0].Supported = false },
	}
	for name, mutate := range mutations {
		t.Run(name, func(t *testing.T) {
			var token preparedCapability
			var proof preparedProvenance
			if err := json.Unmarshal([]byte(capability), &token); err != nil {
				t.Fatal(err)
			}
			if err := json.Unmarshal(token.Payload, &proof); err != nil {
				t.Fatal(err)
			}
			mutate(&proof)
			token.Payload = json.RawMessage(frozenJSON(t, proof))
			if _, err := specializePrepared(snapshot, frozenJSON(t, token), request); err == nil {
				t.Fatal("accepted changed capability component")
			}
		})
	}
}

func TestPreparedConcurrentDifferentKnownFills(t *testing.T) {
	_, snapshot := frozenFixture(t)
	capability := preparedFixture(t, snapshot)
	var workers sync.WaitGroup
	for worker := 0; worker < 8; worker++ {
		workers.Add(1)
		go func() {
			defer workers.Done()
			for iteration := 0; iteration < 10; iteration++ {
				amount := fmt.Sprintf("1844674407370955%04d.123456789", worker*10+iteration)
				options := preparedSpecializationOptions{Projection: frozenPath("config"), Fills: []scalarFill{
					{Path: frozenPath("source.id"), Value: json.RawMessage(`"ok"`)},
					{Path: frozenPath("source.flag"), Value: json.RawMessage(`true`)},
					{Path: frozenPath("source.amount"), Value: json.RawMessage(amount)},
				}}
				result, err := specializePrepared(snapshot, capability, frozenJSON(t, options))
				if err != nil {
					t.Errorf("known specialization failed: %v", err)
					return
				}
				entry := result.Value.(map[string]interface{})["entries"].([]interface{})[0].(map[string]interface{})
				if len(result.Unresolved) != 0 || entry["amount"].(json.Number).String() != amount {
					t.Error("concurrent specialization inherited another fill")
					return
				}
			}
		}()
	}
	workers.Wait()
}

func TestPreparedCapabilityIsProcessLocal(t *testing.T) {
	type request struct{ Snapshot, Capability, Options string }
	if os.Getenv("CUENGINE_PREPARED_CHILD") == "1" {
		var input request
		if err := json.NewDecoder(os.Stdin).Decode(&input); err != nil {
			t.Fatal(err)
		}
		_, err := specializePrepared(input.Snapshot, input.Capability, input.Options)
		if err == nil || err.Error() != "invalid prepared capability" {
			t.Fatal("another process accepted a prepared capability")
		}
		return
	}
	_, snapshot := frozenFixture(t)
	capability := preparedFixture(t, snapshot)
	options := frozenJSON(t, preparedSpecializationOptions{Projection: frozenPath("config"), Deferred: fixtureDeferred()})
	input := frozenJSON(t, request{Snapshot: snapshot, Capability: capability, Options: options})
	command := exec.Command(os.Args[0], "-test.run=^TestPreparedCapabilityIsProcessLocal$")
	command.Env = append(os.Environ(), "CUENGINE_PREPARED_CHILD=1")
	command.Stdin = bytes.NewBufferString(input)
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("capability isolation subprocess failed: %v\n%s", err, output)
	}
}

func TestPreparedSourceScopeMatchesDescendantSemantics(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
source: {id: string, unused: string}
config: id: source.id
`})
	sources := []valuePath{frozenPath("source")}
	preparation := preparationOptions{ReferencePaths: []valuePath{frozenPath("config")}, Projections: []valuePath{frozenPath("config")}, SourcePaths: &sources}
	capability, err := prepareFrozen(snapshot, frozenJSON(t, preparation))
	if err != nil {
		t.Fatal(err)
	}
	request := specializationOptions{Projection: frozenPath("config"), SourcePaths: &sources, Fills: []scalarFill{{Path: frozenPath("source.id"), Value: json.RawMessage(`"ok"`)}}}
	ordinary := frozenPayload(t, specializeFrozenResponse(snapshot, frozenJSON(t, request)))
	prepared := frozenPayload(t, specializePreparedResponse(snapshot, capability, frozenJSON(t, request)))
	if !bytes.Equal(ordinary, prepared) {
		t.Fatal("ancestor source scope changed specialization")
	}
	request.Fills = append(request.Fills, scalarFill{Path: frozenPath("source.unused"), Value: json.RawMessage(`"extra"`)})
	if _, err := specializePrepared(snapshot, capability, frozenJSON(t, request)); err == nil {
		t.Fatal("ancestor source scope authorized an unreferenced fill")
	}
}
