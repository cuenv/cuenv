package main

import (
	"encoding/json"
	"fmt"
	"strings"

	"cuelang.org/go/cue"
	cueerrors "cuelang.org/go/cue/errors"
)

// validateConcretePaths requires each listed top-level field, when present,
// to be fully concrete before the instance is exported.
//
// buildValueClean exports whatever it can decode: an undefined reference, a
// non-concrete value, and a required field missing from a definition all
// become JSON null. Callers name the fields where that would be dangerous
// (for example configuration that drives external side effects) and those
// subtrees are validated with concreteness and finality. cue.All() is
// deliberately not used: it would also demand concrete definitions and
// hidden fields. Fields not listed keep the lenient export.
func validateConcretePaths(v cue.Value, paths []string) error {
	for _, path := range paths {
		field := v.LookupPath(cue.MakePath(cue.Str(path)))
		if !field.Exists() {
			continue
		}
		if err := field.Validate(cue.Concrete(true), cue.Final()); err != nil {
			details := strings.TrimSpace(cueerrors.Details(err, nil))
			return fmt.Errorf("%s: %s", path, details)
		}
	}
	return nil
}

// buildJSONClean builds a JSON representation without any _meta injection.
// This returns clean JSON that can be correlated with the separate meta map.
func buildJSONClean(v cue.Value) ([]byte, error) {
	result := buildValueClean(v)
	return json.Marshal(result)
}

// unquoteSelector strips surrounding quotes from a selector string.
// CUE's Selector.String() returns quoted strings for string-keyed fields,
// e.g., `"test.json"` instead of `test.json`. We need the unquoted form
// for proper JSON serialization and file path handling.
func unquoteSelector(s string) string {
	if len(s) >= 2 && s[0] == '"' && s[len(s)-1] == '"' {
		return s[1 : len(s)-1]
	}
	return s
}

// buildValueClean recursively builds a clean value without metadata
func buildValueClean(v cue.Value) interface{} {
	switch v.Kind() {
	case cue.StructKind:
		result := make(map[string]interface{})
		iter, _ := v.Fields(cue.Definitions(false))
		for iter.Next() {
			sel := iter.Selector()
			fieldName := unquoteSelector(sel.String())
			result[fieldName] = buildValueClean(iter.Value())
		}
		return result

	case cue.ListKind:
		// Use a non-nil slice so empty CUE lists serialize to [] (not null).
		items := make([]interface{}, 0)
		iter, _ := v.List()
		for iter.Next() {
			items = append(items, buildValueClean(iter.Value()))
		}
		return items

	default:
		// Concrete value (string, number, bool, null)
		var val interface{}
		v.Decode(&val)
		return val
	}
}
