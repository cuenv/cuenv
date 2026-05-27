package main

import (
	"encoding/json"

	"cuelang.org/go/cue"
)

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
