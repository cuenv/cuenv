package main

import (
	"encoding/json"
	"errors"
	"strings"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/build"
	cueerrors "cuelang.org/go/cue/errors"
	"cuelang.org/go/cue/load"
)

// errorDetails renders every error in err with its source positions, as the
// cue command line tool prints them. Positions are relative to moduleRoot.
// A plain %v would print only the first error of a list ("… (and 3 more
// errors)") and no positions.
func errorDetails(err error, moduleRoot string) string {
	return strings.TrimSpace(cueerrors.Details(err, &cueerrors.Config{Cwd: moduleRoot, ToSlash: true}))
}

// instanceFailure formats one failed instance as "<key>: <details>", with
// every continuation line indented so each instance's block stays together
// in a list of failures.
func instanceFailure(key string, err error, moduleRoot string) string {
	details := errorDetails(err, moduleRoot)
	return key + ": " + strings.ReplaceAll(details, "\n", "\n  ")
}

// allErrors returns every error of a value whose Err is set. Err reports the
// first error that made the value fail; Validate collects them all (for
// example one per misspelled field), which is what a user fixing the file
// needs to see. Incomplete values are not errors here: they are reported by
// concretePaths or appear as null in the lenient export, as before.
func allErrors(v cue.Value) error {
	if err := v.Validate(); err != nil {
		return err
	}
	return v.Err()
}

// exportsTasks reports whether an export includes the "tasks" field: no
// projection (the whole instance), or a projected path starting at "tasks".
func exportsTasks(exportPaths []projectionPath) bool {
	if len(exportPaths) == 0 {
		return true
	}
	for _, projection := range exportPaths {
		if projection.labels[0] == "tasks" {
			return true
		}
	}
	return false
}

// emptyResultResponse is the success envelope for a query that matched no
// instance, keeping reported skipped directories.
func emptyResultResponse(skipped []SkippedDirectory) string {
	payload, err := json.Marshal(ModuleResult{
		Instances:          map[string]json.RawMessage{},
		Projects:           []string{},
		SkippedDirectories: skipped,
	})
	if err != nil {
		return createErrorResponse(ErrorCodeJSONMarshal, "Failed to marshal module result: "+err.Error(), nil)
	}
	return createSuccessResponse(string(payload))
}

// withoutEmptyInstances drops instances without CUE files of their own.
//
// CUE's "./..." walk leaves directories without files out silently; an
// explicit directory list (see recursiveDirectories) returns them as
// NoFilesError instances instead. Loading every package ("*") also yields an
// anonymous ("_") instance with no files next to each package directory;
// it would only ever evaluate to an empty value.
func withoutEmptyInstances(instances []*build.Instance) []*build.Instance {
	kept := instances[:0]
	for _, inst := range instances {
		var noFiles *load.NoFilesError
		if inst.Err != nil && len(inst.InvalidFiles) == 0 && errors.As(inst.Err, &noFiles) {
			continue
		}
		if inst.Err == nil && inst.PkgName == "_" && len(inst.Files) == 0 && len(inst.BuildFiles) == 0 {
			continue
		}
		kept = append(kept, inst)
	}
	return kept
}
