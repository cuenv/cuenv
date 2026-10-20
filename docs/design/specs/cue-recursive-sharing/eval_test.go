package evidence

import (
	"fmt"
	"os"
	"testing"
	"time"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/errors"
	"cuelang.org/go/cue/load"
	"cuelang.org/go/internal/core/adt"
)

func TestEvidence(t *testing.T) {
	root := os.Getenv("EVIDENCE_ROOT")
	adt.ResetStats()
	start := time.Now()
	instances := load.Instances([]string{"."}, &load.Config{
		Dir: root, ModuleRoot: root, Package: "cuenv",
	})
	if len(instances) != 1 {
		t.Fatalf("loaded %d instances", len(instances))
	}
	value := cuecontext.New().BuildInstance(instances[0])
	err := value.Validate(cue.Concrete(false))
	elapsed := time.Since(start).Seconds()
	counts := adt.TotalStats()
	fmt.Printf("EVIDENCE seconds=%.6f unifications=%d disjuncts=%d error=%s\n",
		elapsed, counts.Unifications, counts.Disjuncts, errors.Details(err, nil))
	if err != nil {
		t.Fatal(err)
	}
}
