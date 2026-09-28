package main

import (
	"encoding/json"
	"fmt"
	"strings"

	"cuelang.org/go/cue"
)

// concretePath is a caller-named path that must evaluate to a fully concrete
// value. text is the path as the caller wrote it and is used in errors.
type concretePath struct {
	text string
	path cue.Path
}

// parseConcretePaths parses caller-named paths with CUE path syntax, so
// nested selectors ("a.b"), quoted labels ("\"my-field\".b") and list
// indices ("a[0]") are supported. An empty or malformed path is an input
// error: silently ignoring it would disable the check the caller asked for.
func parseConcretePaths(texts []string) ([]concretePath, error) {
	paths := make([]concretePath, 0, len(texts))
	for _, text := range texts {
		if strings.TrimSpace(text) == "" {
			return nil, fmt.Errorf("concrete path must not be empty")
		}
		path := cue.ParsePath(text)
		if err := path.Err(); err != nil {
			return nil, fmt.Errorf("concrete path %q is not a valid CUE path: %v", text, err)
		}
		paths = append(paths, concretePath{text: text, path: path})
	}
	return paths, nil
}

// validateConcretePaths requires every listed path to exist and to be fully
// concrete before the instance is exported.
//
// buildValueClean exports whatever it can decode: an undefined reference, a
// non-concrete value, and a required field missing from a definition all
// become JSON null. Callers name the paths where that would be dangerous
// (for example configuration that drives external side effects) and those
// subtrees are validated with concreteness and finality. cue.All() is
// deliberately not used: it would also demand concrete definitions and
// hidden fields. Paths not listed keep the lenient export.
//
// The check fails closed: a listed path that does not exist is an error,
// because a caller that names a path relies on it being validated.
func validateConcretePaths(v cue.Value, paths []concretePath, moduleRoot string) error {
	for _, concrete := range paths {
		field := v.LookupPath(concrete.path)
		if !field.Exists() {
			if err := field.Err(); err != nil {
				return fmt.Errorf("%s: concrete path does not exist: %s", concrete.text, errorDetails(err, moduleRoot))
			}
			return fmt.Errorf("%s: concrete path does not exist", concrete.text)
		}
		if err := field.Validate(cue.Concrete(true), cue.Final()); err != nil {
			return fmt.Errorf("%s: %s", concrete.text, errorDetails(err, moduleRoot))
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
