package main

import (
	"encoding/json"
	"fmt"
	"runtime"
	"strings"
	"testing"
	"time"
)

// This opt-in benchmark measures one consumer subtree, not a multi-resource
// stack. It has 35 instance nodes (the selected root and 34 imports), 64 nested entries and 192 scalar
// reference leaves. The bridge still has no hard time or memory bounds.
// Run with: go test -run '^$' -bench '^BenchmarkFrozenImportedGraph$' -benchtime=24x -count=1 -benchmem
func BenchmarkFrozenImportedGraph(b *testing.B) {
	const layers, entries = 32, 64
	files := map[string]string{
		"shared/types.cue": "package shared\n#Common: {flag: bool, ...}\n",
		"layer0/types.cue": "package layer0\n#Entry: {id: string, amount: number, flag: bool}\n",
	}
	for index := 1; index <= layers; index++ {
		files[fmt.Sprintf("layer%d/types.cue", index)] = fmt.Sprintf("package layer%d\nimport (\nprevious \"example.com/bridge/layer%d\"\n\"example.com/bridge/shared\"\n)\n#Entry: previous.#Entry & shared.#Common\n", index, index-1)
	}
	items := make([]string, entries)
	for index := range items {
		items[index] = "schema.#Entry & {id: source.id, amount: source.amount, flag: source.flag}"
	}
	files["env.cue"] = fmt.Sprintf("package app\nimport schema \"example.com/bridge/layer%d\"\nsource: {id: string, amount: number, flag: bool}\nconfig: a: b: c: d: e: f: g: entries: [%s]\n", layers, strings.Join(items, ","))
	_, snapshot := freezeTestModule(b, files)
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		b.Fatal(err)
	}
	if len(graph.Nodes) != layers+3 {
		b.Fatalf("shared imports were duplicated: %d", len(graph.Nodes))
	}
	sourceBytes := 0
	for _, node := range graph.Nodes {
		for _, file := range node.Files {
			sourceBytes += len(file.Source)
		}
	}
	options := frozenJSON(b, specializationOptions{Projection: frozenPath("config"), Fills: []scalarFill{
		{Path: frozenPath("source.id"), Value: json.RawMessage(`"ok"`)},
		{Path: frozenPath("source.amount"), Value: json.RawMessage("9007199254740993.125")},
		{Path: frozenPath("source.flag"), Value: json.RawMessage("true")},
	}})
	frozenPayload(b, specializeFrozenResponse(snapshot, options))
	b.ReportAllocs()
	runtime.GC()
	var before, after, current runtime.MemStats
	runtime.ReadMemStats(&before)
	peak := before.HeapAlloc
	start := time.Now()
	for b.Loop() {
		frozenPayload(b, specializeFrozenResponse(snapshot, options))
		runtime.ReadMemStats(&current)
		if current.HeapAlloc > peak {
			peak = current.HeapAlloc
		}
	}
	elapsed := time.Since(start)
	runtime.GC()
	runtime.ReadMemStats(&after)
	b.ReportMetric(float64(sourceBytes), "source_bytes")
	b.ReportMetric(float64(len(snapshot)), "snapshot_bytes")
	b.ReportMetric(float64(peak), "sampled_heap_bytes")
	b.ReportMetric(float64(after.HeapAlloc)-float64(before.HeapAlloc), "retained_delta_bytes")
	b.Logf("instance_nodes=%d consumer_subtrees=1 nested_entries=%d reference_leaves=%d repetitions=%d source_bytes=%d snapshot_bytes=%d elapsed=%s sampled_peak_heap=%d retained_before=%d retained_after=%d total_allocated=%d", len(graph.Nodes), entries, entries*3, b.N, sourceBytes, len(snapshot), elapsed, peak, before.HeapAlloc, after.HeapAlloc, after.TotalAlloc-before.TotalAlloc)
}
