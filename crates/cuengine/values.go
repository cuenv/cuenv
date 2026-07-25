package main

import (
	"encoding/json"
	"fmt"

	"cuelang.org/go/cue"
)

// buildJSONClean builds a JSON representation without any _meta injection.
// This returns clean JSON that can be correlated with the separate meta map.
func buildJSONClean(v cue.Value) ([]byte, error) {
	if err := v.Validate(cue.Concrete(true)); err != nil {
		return nil, err
	}
	result, err := buildValueClean(v)
	if err != nil {
		return nil, err
	}
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
func buildValueClean(v cue.Value) (interface{}, error) {
	switch v.Kind() {
	case cue.StructKind:
		result := make(map[string]interface{})
		iter, err := v.Fields(cue.Definitions(false))
		if err != nil {
			return nil, err
		}
		for iter.Next() {
			sel := iter.Selector()
			fieldName := unquoteSelector(sel.String())
			fieldValue, err := buildValueClean(iter.Value())
			if err != nil {
				return nil, fmt.Errorf("field %s: %w", fieldName, err)
			}
			result[fieldName] = fieldValue
		}
		return result, nil

	case cue.ListKind:
		// Use a non-nil slice so empty CUE lists serialize to [] (not null).
		items := make([]interface{}, 0)
		iter, err := v.List()
		if err != nil {
			return nil, err
		}
		for iter.Next() {
			item, err := buildValueClean(iter.Value())
			if err != nil {
				return nil, fmt.Errorf("list item %d: %w", len(items), err)
			}
			items = append(items, item)
		}
		return items, nil

	default:
		// Concrete value (string, number, bool, null)
		var val interface{}
		if err := v.Decode(&val); err != nil {
			return nil, err
		}
		return val, nil
	}
}
