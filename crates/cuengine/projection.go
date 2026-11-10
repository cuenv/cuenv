package main

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"

	"cuelang.org/go/cue"
)

// projectionPath is a caller-named field path used by exportPaths or
// presencePaths. text is the path as the caller wrote it; it keys results
// and appears in errors.
type projectionPath struct {
	text   string
	path   cue.Path
	labels []string
}

// parseProjectionPaths parses caller-named paths with CUE path syntax. Only
// regular field selectors are allowed ("name", "a.b", "\"my-field\".c"):
// the projection rebuilds the exported object from the labels, which list
// indices, definitions, hidden fields and pattern constraints cannot name.
func parseProjectionPaths(option string, texts []string) ([]projectionPath, error) {
	paths := make([]projectionPath, 0, len(texts))
	for _, text := range texts {
		if strings.TrimSpace(text) == "" {
			return nil, fmt.Errorf("%s entries must not be empty", option)
		}
		path := cue.ParsePath(text)
		if err := path.Err(); err != nil {
			return nil, fmt.Errorf("%s entry %q is not a valid CUE path: %v", option, text, err)
		}
		selectors := path.Selectors()
		labels := make([]string, 0, len(selectors))
		for _, selector := range selectors {
			if selector.LabelType() != cue.StringLabel || selector.ConstraintType() != 0 {
				return nil, fmt.Errorf("%s entry %q must name regular fields only (no list indices, definitions, hidden fields or constraints)", option, text)
			}
			labels = append(labels, selector.Unquoted())
		}
		paths = append(paths, projectionPath{text: text, path: path, labels: labels})
	}
	return paths, nil
}

// buildProjectedJSON exports only the listed paths of v, nested as they are
// in v: exporting "a.b" and "c" yields {"a":{"b":…},"c":…}. A path that does
// not exist is left out. Nothing outside the listed paths is exported, which
// keeps the result (and the work to produce it) proportional to what the
// caller asked for.
func buildProjectedJSON(v cue.Value, paths []projectionPath) ([]byte, error) {
	// Shorter paths first, so a path exported whole is not replaced by the
	// partial object of a longer path below it.
	ordered := append([]projectionPath{}, paths...)
	sort.SliceStable(ordered, func(i, j int) bool { return len(ordered[i].labels) < len(ordered[j].labels) })
	result := make(map[string]interface{})
	for _, projection := range ordered {
		field := v.LookupPath(projection.path)
		if !field.Exists() {
			continue
		}
		setNested(result, projection.labels, buildValueClean(field))
	}
	return json.Marshal(result)
}

// setNested stores value under labels, creating intermediate objects. A
// value already stored at or above labels is kept.
func setNested(target map[string]interface{}, labels []string, value interface{}) {
	for index, label := range labels {
		if index == len(labels)-1 {
			if _, exists := target[label]; !exists {
				target[label] = value
			}
			return
		}
		next, ok := target[label].(map[string]interface{})
		if !ok {
			if _, exists := target[label]; exists {
				return
			}
			next = make(map[string]interface{})
			target[label] = next
		}
		target = next
	}
}

// presentPaths lists which of the paths exist in v, without evaluating or
// exporting their values beyond what existence needs.
func presentPaths(v cue.Value, paths []projectionPath) []string {
	present := []string{}
	for _, projection := range paths {
		if v.LookupPath(projection.path).Exists() {
			present = append(present, projection.text)
		}
	}
	return present
}
