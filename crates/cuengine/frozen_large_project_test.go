package main

import (
	"encoding/json"
	"os"
	"runtime"
	"testing"
	"time"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/provenance"
)

// testdata/large_project.json is cuenv's own project frozen, with its recursive task
// schema and generated workflows: nothing simpler showed these costs. Asking about
// part of a project must not cost what the rest of it would, and asking about all of
// it must still finish: this ran out of memory at 37 GB.
func TestFrozenAnalysisOfALargeProject(t *testing.T) {
	encoded, err := os.ReadFile("testdata/large_project.json")
	if err != nil {
		t.Fatal(err)
	}
	var frozen frozenGraph
	if err := json.Unmarshal(encoded, &frozen); err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name       string
		root       cue.Path
		allocation uint64
	}{
		{"one field", cue.ParsePath("name"), 16 << 20},
		{"one task", cue.ParsePath("tasks.checks.cuenv.command"), 256 << 20},
		{"the generated workflows", cue.ParsePath("ci"), 8 << 30},
		{"everything", cue.Path{}, 8 << 30},
	} {
		t.Run(test.name, func(t *testing.T) {
			instance, err := frozen.instance()
			if err != nil {
				t.Fatal(err)
			}
			var before, after runtime.MemStats
			runtime.ReadMemStats(&before)
			finished := make(chan error, 1)
			go func() {
				_, err := provenance.Analyze(instance, test.root)
				finished <- err
			}()
			select {
			case err := <-finished:
				if err != nil {
					t.Fatal(err)
				}
			case <-time.After(2 * time.Minute):
				t.Fatal("analysis did not finish")
			}
			runtime.ReadMemStats(&after)
			if allocated := after.TotalAlloc - before.TotalAlloc; allocated > test.allocation {
				t.Errorf("allocated %d MB, more than %d MB", allocated>>20, test.allocation>>20)
			}
		})
	}
}
