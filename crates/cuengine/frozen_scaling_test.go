package main

import (
	"encoding/json"
	"fmt"
	"os"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"
)

type scalingCase struct {
	snapshot        string
	capability      string
	unknown         []string
	known           []string
	nodes           int
	sourceBytes     int
	referenceLeaves int
	referenceRoots  int
	sourcePaths     int
	setup           time.Duration
}

func scalingFixture(t *testing.T, consumers, layers int) scalingCase {
	t.Helper()
	started := time.Now()
	files := map[string]string{
		"shared/types.cue": "package shared\n#Common: {flag: bool, ...}\n",
		"layer0/types.cue": "package layer0\n#Entry: {id: string, amount: number, flag: bool}\n",
	}
	for index := 1; index <= layers; index++ {
		files[fmt.Sprintf("layer%d/types.cue", index)] = fmt.Sprintf("package layer%d\nimport (\nprevious \"example.com/bridge/layer%d\"\n\"example.com/bridge/shared\"\n)\n#Entry: previous.#Entry & shared.#Common\n", index, index-1)
	}
	selected := "infrastructure.environments.dev"
	ordinary := `configuration: {label: "ordinary", nested: schema.#Entry & {id: "fixed", amount: 1, flag: true}}`
	producer := `{type: "fake_source", outputs: {id: string, amount: number, flag: bool}, configuration: name: "producer"}`
	var managed, data strings.Builder
	fmt.Fprintf(&managed, "producer: %s\n", producer)
	fmt.Fprintf(&data, "producer: %s & {dependsOn: [\"producer\"]}\n", producer)
	projections := make([]valuePath, consumers)
	sources := make([]string, consumers)
	deferred := make([][]deferredValue, consumers)
	allDeferred := make([]deferredValue, 0, consumers*3)
	for index := 0; index < consumers; index++ {
		namespace, sourceNamespace := "resources", "resources"
		if index%2 != 0 {
			namespace, sourceNamespace = "data", "data"
		}
		name := fmt.Sprintf("consumer_%03d", index)
		source := selected + "." + sourceNamespace + ".producer.outputs"
		declaration := fmt.Sprintf("%s: {type: \"fake_consumer\", outputs: {id: string, amount: number, flag: bool}, configuration: {label: \"ordinary-%d\", nested: schema.#Entry & {id: %s.id, amount: %s.amount, flag: %s.flag}}}\n", name, index, source, source, source)
		if namespace == "resources" {
			managed.WriteString(declaration)
		} else {
			data.WriteString(declaration)
		}
		projection := selected + "." + namespace + "." + name + ".configuration"
		projections[index] = frozenPath(projection)
		sources[index] = source
		for _, attribute := range []string{"id", "amount", "flag"} {
			entry := deferredValue{Path: frozenPath(projection + ".nested." + attribute), Target: frozenPath(source + "." + attribute)}
			deferred[index] = append(deferred[index], entry)
			allDeferred = append(allDeferred, entry)
		}
	}
	files["env.cue"] = fmt.Sprintf(`package app
import schema "example.com/bridge/layer%d"
name: "scaling-review"
env: ORDINARY: "literal"
cache: {}
infrastructure: {
 state: turso: {url: "http://unused", token: "synthetic-state-token"}
 workflows: {}
 providers: fake: {source: "example/fake", path: "/not-started"}
 providerEnvironment: "isolated"
 resources: {producer: %s, inactive: {type: "fake_consumer", %s}}
 data: {}
 environments: {
  dev: {
   providers: fake: {source: "example/fake", path: "/not-started"}
   providerEnvironment: "isolated"
   resources: {%s}
   data: {%s}
  }
  prod: {
   providers: fake: {source: "example/fake", path: "/not-started"}
   providerEnvironment: "isolated"
   resources: {producer: %s, inactive: {type: "fake_consumer", %s}}
   data: {}
  }
 }
}
`, layers, producer, ordinary, managed.String(), data.String(), producer, ordinary)
	_, snapshot := freezeTestModule(t, files)
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		t.Fatal(err)
	}
	sourceBytes := 0
	for _, node := range graph.Nodes {
		for _, file := range node.Files {
			sourceBytes += len(file.Source)
		}
	}
	roots := []valuePath{}
	for _, namespace := range []string{"providers", "resources", "data", "providerEnvironment"} {
		roots = append(roots, frozenPath(selected+"."+namespace))
	}
	for _, field := range []string{"name", "env", "cache"} {
		roots = append(roots, frozenPath(field))
	}
	for _, field := range []string{"state", "migrateStateTo", "workflows"} {
		roots = append(roots, frozenPath("infrastructure."+field))
	}
	sourcePaths := []valuePath{}
	for _, source := range []string{selected + ".resources.producer.outputs", selected + ".data.producer.outputs", "infrastructure.resources.producer.outputs", "infrastructure.environments.prod.resources.producer.outputs"} {
		for _, attribute := range []string{"id", "amount", "flag"} {
			sourcePaths = append(sourcePaths, frozenPath(source+"."+attribute))
		}
	}
	abstractPaths := []valuePath{frozenPath(selected + ".resources.producer.outputs"), frozenPath(selected + ".data.producer.outputs")}
	for index := 0; index < consumers; index++ {
		namespace := "resources"
		if index%2 != 0 {
			namespace = "data"
		}
		output := fmt.Sprintf("%s.%s.consumer_%03d.outputs", selected, namespace, index)
		abstractPaths = append(abstractPaths, frozenPath(output))
		for _, attribute := range []string{"id", "amount", "flag"} {
			sourcePaths = append(sourcePaths, frozenPath(output+"."+attribute))
		}
	}
	ignored := []valuePath{frozenPath("infrastructure.environments.prod")}
	ignoredGroups := [][]valuePath{{frozenPath("infrastructure.providers"), frozenPath("infrastructure.resources"), frozenPath("infrastructure.data"), frozenPath("infrastructure.providerEnvironment")}}
	inspection := frozenEvaluationOptions{ExportPaths: []valuePath{frozenPath("name"), frozenPath("env"), frozenPath("cache"), frozenPath("infrastructure")}, ReferencePaths: roots, SourcePaths: &sourcePaths, IgnoredPaths: ignored, IgnoredGroups: ignoredGroups}
	inspected := frozenPayload(t, evaluateFrozenResponse(snapshot, frozenJSON(t, inspection)))
	var result frozenEvaluation
	if err := json.Unmarshal(inspected, &result); err != nil {
		t.Fatal(err)
	}
	count := 0
	for _, reference := range result.References {
		if reference.PrimitiveType != "" && reference.Supported {
			count++
		}
	}
	if count != consumers*3 {
		t.Fatalf("expected %d reference leaves, got %d", consumers*3, count)
	}
	validation := frozenValidationOptions{ConcretePaths: []valuePath{frozenPath("infrastructure.state"), frozenPath(selected)}, AbstractPaths: abstractPaths, Deferred: allDeferred, SourcePaths: &sourcePaths, IgnoredPaths: ignored, IgnoredGroups: ignoredGroups}
	frozenPayload(t, validateFrozenResponse(snapshot, frozenJSON(t, validation)))
	fixture := scalingCase{snapshot: snapshot, nodes: len(graph.Nodes), sourceBytes: sourceBytes, referenceLeaves: count, referenceRoots: len(roots), sourcePaths: len(sourcePaths)}
	if os.Getenv("CUENV_FROZEN_SCALING_VARIANT") == "prepared" {
		options := preparationOptions{ReferencePaths: roots, SourcePaths: &sourcePaths, IgnoredPaths: ignored, IgnoredGroups: ignoredGroups, Projections: projections}
		capability := frozenPayload(t, prepareFrozenResponse(snapshot, frozenJSON(t, options)))
		if err := json.Unmarshal(capability, &fixture.capability); err != nil {
			t.Fatal(err)
		}
	}
	for index, projection := range projections {
		options := specializationOptions{Projection: projection, Deferred: deferred[index], ReferencePaths: roots, SourcePaths: &sourcePaths, IgnoredPaths: ignored, IgnoredGroups: ignoredGroups}
		if fixture.capability == "" {
			fixture.unknown = append(fixture.unknown, frozenJSON(t, options))
		} else {
			fixture.unknown = append(fixture.unknown, frozenJSON(t, preparedSpecializationOptions{Projection: projection, Deferred: deferred[index]}))
		}
		options.Deferred = nil
		options.Fills = []scalarFill{
			{Path: frozenPath(sources[index] + ".id"), Value: json.RawMessage(`"synthetic-source-id"`)},
			{Path: frozenPath(sources[index] + ".amount"), Value: json.RawMessage(`9007199254740993.125`)},
			{Path: frozenPath(sources[index] + ".flag"), Value: json.RawMessage(`true`)},
		}
		if fixture.capability == "" {
			fixture.known = append(fixture.known, frozenJSON(t, options))
		} else {
			fixture.known = append(fixture.known, frozenJSON(t, preparedSpecializationOptions{Projection: projection, Fills: options.Fills}))
		}
	}
	fixture.setup = time.Since(started)
	return fixture
}

func TestScalingReviewProductionScope(t *testing.T) {
	if os.Getenv("CUENV_FROZEN_SCALING") != "1" {
		t.Skip("set CUENV_FROZEN_SCALING=1 to run the scaling experiment")
	}
	counts := []int{1, 8, 32, 64}
	if text := os.Getenv("SCALING_CONSUMERS"); text != "" {
		counts = nil
		for _, field := range strings.Split(text, ",") {
			value, err := strconv.Atoi(field)
			if err != nil {
				t.Fatal(err)
			}
			counts = append(counts, value)
		}
	}
	layers := 4
	if text := os.Getenv("SCALING_LAYERS"); text != "" {
		value, err := strconv.Atoi(text)
		if err != nil {
			t.Fatal(err)
		}
		layers = value
	}
	repetitions := 3
	if text := os.Getenv("SCALING_REPETITIONS"); text != "" {
		value, err := strconv.Atoi(text)
		if err != nil {
			t.Fatal(err)
		}
		repetitions = value
	}
	t.Logf("environment go=%s gomaxprocs=%d numcpu=%d layers=%d repetitions=%d", runtime.Version(), runtime.GOMAXPROCS(0), runtime.NumCPU(), layers, repetitions)
	for _, consumers := range counts {
		t.Run(fmt.Sprintf("consumers_%03d", consumers), func(t *testing.T) {
			runtime.GC()
			var setupBefore, setupAfter runtime.MemStats
			runtime.ReadMemStats(&setupBefore)
			fixture := scalingFixture(t, consumers, layers)
			runtime.ReadMemStats(&setupAfter)
			metadata := map[string]any{"event": "fixture", "consumers": consumers, "layers": layers, "instance_nodes": fixture.nodes, "source_bytes": fixture.sourceBytes, "snapshot_bytes": len(fixture.snapshot), "reference_leaves": fixture.referenceLeaves, "reference_roots": fixture.referenceRoots, "source_paths": fixture.sourcePaths, "ignored_subtrees": 1, "ignored_groups": 1, "setup_ms": float64(fixture.setup.Microseconds()) / 1000, "setup_allocated_bytes": setupAfter.TotalAlloc - setupBefore.TotalAlloc, "variant": os.Getenv("CUENV_FROZEN_SCALING_VARIANT")}
			encoded, _ := json.Marshal(metadata)
			t.Log(string(encoded))
			for repetition := 0; repetition < repetitions; repetition++ {
				for _, phase := range []struct {
					name     string
					requests []string
				}{{"unknown_preflight", fixture.unknown}, {"unknown_plan", fixture.unknown}, {"known_apply", fixture.known}} {
					runtime.GC()
					var before, after, current runtime.MemStats
					runtime.ReadMemStats(&before)
					sampledPeak := before.HeapAlloc
					started := time.Now()
					for _, request := range phase.requests {
						var response string
						if fixture.capability == "" {
							response = specializeFrozenResponse(fixture.snapshot, request)
						} else {
							response = specializePreparedResponse(fixture.snapshot, fixture.capability, request)
						}
						raw := frozenPayload(t, response)
						var result specializationResult
						if err := json.Unmarshal(raw, &result); err != nil {
							t.Fatal(err)
						}
						expected := 0
						if phase.name != "known_apply" {
							expected = 3
						}
						if len(result.Unresolved) != expected {
							t.Fatalf("wrong unresolved count %d", len(result.Unresolved))
						}
						runtime.ReadMemStats(&current)
						if current.HeapAlloc > sampledPeak {
							sampledPeak = current.HeapAlloc
						}
					}
					elapsed := time.Since(started)
					runtime.ReadMemStats(&after)
					allocated := after.TotalAlloc - before.TotalAlloc
					allocations := after.Mallocs - before.Mallocs
					gcCycles := after.NumGC - before.NumGC
					runtime.GC()
					runtime.ReadMemStats(&current)
					row := map[string]any{"event": "phase", "consumers": consumers, "layers": layers, "repetition": repetition + 1, "phase": phase.name, "calls": len(phase.requests), "elapsed_ms": float64(elapsed.Microseconds()) / 1000, "ns_per_call": elapsed.Nanoseconds() / int64(len(phase.requests)), "allocated_bytes": allocated, "allocations": allocations, "sampled_peak_heap_bytes": sampledPeak, "retained_heap_bytes": current.HeapAlloc, "retained_delta_bytes": int64(current.HeapAlloc) - int64(before.HeapAlloc), "gc_cycles": gcCycles}
					encoded, _ := json.Marshal(row)
					t.Log(string(encoded))
				}
			}
		})
	}
}
