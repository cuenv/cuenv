package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"
	"testing"

	"cuelang.org/go/cue"
)

// Controls take a reference expression X: "tainted" passes an output, "unrelated"
// passes a concrete hidden field. Wrappers are syntax which hid controls before.
var generatedControls = map[string]string{
	"label_default": `{key: "k", (*(X & "x") | "mode"): "v"}`,
	"label_plain":   `{key: "k", (X): "v"}`,
	"label_interp":  `{key: "k", "\(X)": "v"}`,
	"pattern":       `{key: "k", [X & "x"]: string}`,
	"if_narrowed":   `{key: "k", if (X & "x") == "x" {a: "v"}}`,
	"if_equal":      `{key: "k", if X == "x" {a: "v"}}`,
	"if_presence":   `{key: "k", if (X & =~"^x") != _|_ {a: "v"}}`,
	"for_list":      `{key: "k", for v in [X & "x"] {(v): "v"}}`,
	"nested_if":     `{key: "k", if true {if (X & "x") == "x" {a: "v"}}}`,
	"let_clause": `{key: "k", let L = X & "x"
 if L == "x" {a: "v"}}`,
	"index_value": `{key: "k", a: {x: "v"}[X]}`,
}

var generatedWrappers = map[string]string{
	"bare":          `C`,
	"close":         `close(C)`,
	"close_twice":   `close(close(C))`,
	"paren":         `(C)`,
	"conjunction":   `{key: "k"} & C`,
	"conj_close":    `{key: "k"} & close(C)`,
	"embed":         `{C}`,
	"embed_twice":   `{{C}}`,
	"default_first": `*C | {key: "z"}`,
	"default_last":  `{key: "z"} | *C`,
	"default_close": `*close(C) | {key: "z"}`,
	"and":           `and([C, {key: "k"}])`,
	"or":            `or([C])`,
	"selector":      `({inner: C}).inner`,
	"index":         `[C][0]`,
	"nested_field":  `{wrap: C}`,
	"list_field":    `{items: [C]}`,
	"if_true":       `{if true {wrap: C}}`,
	"for_once":      `{for _ in [1] {wrap: C}}`,
	"let_field": `{let D = C
 wrap: D}`,
	"def_field": `{#D: C
 wrap: #D}`,
}

type generatedCase struct{ control, wrapper, placement, reference string }

func generatedModule(test generatedCase) map[string]string {
	config := strings.ReplaceAll(test.wrapper, "C", strings.ReplaceAll(test.control, "X", test.reference))
	switch test.placement {
	case "local":
		return map[string]string{"env.cue": "package app\n" + matrixBase + "_u: \"x\"\n" + matrixConfig + config + "\n"}
	case "data":
		return map[string]string{"env.cue": "package app\n" + matrixBase + "_u: \"x\"\nsystem: lookups: lookup: settings: " + config + "\n"}
	case "hidden_binding":
		return map[string]string{"env.cue": "package app\n" + matrixBase + "_u: \"x\"\n_held: " + config + "\n" + matrixConfig + "_held\n"}
	case "let_binding":
		return map[string]string{"env.cue": "package app\n" + matrixBase + "_u: \"x\"\nlet Held = " + config + "\n" + matrixConfig + "Held\n"}
	}
	// imported: the control and its names live in another package.
	return map[string]string{
		"env.cue":          "package app\n" + matrixImport + "system: schema.#S\n",
		"schema/types.cue": "package schema\n#S: {units: {producer: {outputs: id: string}, consumer: settings: " + config + "}, _i: units.producer.outputs.id, _u: \"x\"}\n",
	}
}

func generatedRead(value cue.Value, path cue.Path, out map[string]string) {
	if len(path.Selectors()) > 64 {
		return
	}
	switch value.Kind() {
	case cue.StructKind:
		fields, err := value.Fields(cue.Hidden(true))
		if err != nil {
			out[path.String()] = "error"
			return
		}
		for fields.Next() {
			generatedRead(fields.Value(), cue.MakePath(append(append([]cue.Selector{}, path.Selectors()...), fields.Selector())...), out)
		}
	case cue.ListKind:
		_ = visitChildren(value, path, func(child cue.Value, childPath cue.Path) error {
			generatedRead(child, childPath, out)
			return nil
		})
	default:
		encoded, err := value.MarshalJSON()
		if err != nil {
			out[path.String()] = "unset"
			return
		}
		out[path.String()] = string(encoded)
	}
}

// fillChanges is an oracle, never an authority: it can only show a dependency.
func fillChanges(t *testing.T, snapshot string, edges []string) string {
	t.Helper()
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		t.Fatal(err)
	}
	var baseline map[string]string
	var first string
	for _, fill := range []string{"x", "mode", "version", "plain-1", "", "v"} {
		value, err := graph.build()
		if err != nil {
			t.Fatal(err)
		}
		for _, source := range matrixSources(t, snapshot) {
			path, _ := source.cuePath()
			value = value.FillPath(path, fill)
		}
		// A value CUE refuses stops the run, which changes nothing.
		if value.Validate() != nil {
			continue
		}
		read := map[string]string{}
		generatedRead(value, cue.Path{}, read)
		for key := range read {
			if strings.Contains(key, ".outputs.") || strings.Contains(key, "._") {
				delete(read, key)
			}
			for _, edge := range edges {
				if strings.HasPrefix(edge, key+"<-") || strings.HasPrefix(edge, key+"<=") {
					delete(read, key)
				}
			}
		}
		if baseline == nil {
			baseline, first = read, fill
			continue
		}
		var differing []string
		firstEncoded, _ := json.Marshal(first)
		fillEncoded, _ := json.Marshal(fill)
		for key, entry := range read {
			// A leaf tracking the fill exactly is a pure alias of the source.
			alias := baseline[key] == string(firstEncoded) && entry == string(fillEncoded) && !(strings.HasPrefix(key, "system.") && strings.Contains(key, ".settings."))
			if baseline[key] != entry && !alias {
				differing = append(differing, key)
			}
		}
		for key := range baseline {
			if _, present := read[key]; !present {
				differing = append(differing, key)
			}
		}
		if len(differing) > 0 {
			sort.Strings(differing)
			return fmt.Sprintf("fills %q and %q differ at %v", first, fill, differing)
		}
	}
	return ""
}

func TestFrozenGeneratedControls(t *testing.T) {
	type total struct{ detected, incomplete, clean, inspection, invalid int }
	var tainted, unrelated total
	var escapes, falseRejections, panics []string
	for controlName, control := range generatedControls {
		for wrapperName, wrapper := range generatedWrappers {
			for _, placement := range []string{"local", "data", "hidden_binding", "let_binding", "imported"} {
				name := placement + "/" + wrapperName + "/" + controlName
				output := matrixSource
				if placement == "imported" {
					output = "_i"
				}
				try := func(reference string) (verdict string, edges []string, snapshot string, err error) {
					defer func() {
						if recovered := recover(); recovered != nil {
							verdict, err = "panic", nil
						}
					}()
					dir := writeCueModule(t, generatedModule(generatedCase{control, wrapper, placement, reference}))
					response := freezeInstanceResponse(frozenJSON(t, freezeOptions{ModuleRoot: dir, Directory: dir, Package: "app"}))
					if strings.Contains(response, `"error"`) {
						return "unbuildable", nil, "", nil
					}
					snapshot = string(frozenPayload(t, response))
					var graph frozenGraph
					if json.Unmarshal([]byte(snapshot), &graph) != nil {
						t.Fatal("snapshot")
					}
					if _, err := graph.build(); err != nil {
						return "unbuildable", nil, "", nil
					}
					verdict, edges, err = matrixVerdict(t, snapshot)
					return verdict, edges, snapshot, err
				}
				verdict, edges, snapshot, err := try(output)
				switch {
				case errors.Is(err, errInspection):
					tainted.inspection++
				case verdict == "detected":
					tainted.detected++
				case verdict == "incomplete":
					tainted.incomplete++
				case verdict == "unbuildable":
					tainted.invalid++
				case verdict == "panic":
					panics = append(panics, "tainted "+name)
				default:
					tainted.clean++
					if change := fillChanges(t, snapshot, edges); change != "" {
						escapes = append(escapes, fmt.Sprintf("%s verdict=%s %v: %s", name, verdict, edges, change))
					}
				}
				verdict, _, _, err = try("_u")
				switch {
				case errors.Is(err, errInspection):
					unrelated.inspection++
					falseRejections = append(falseRejections, name+": "+err.Error())
				case verdict == "detected", verdict == "incomplete":
					unrelated.detected++
					falseRejections = append(falseRejections, name+": "+verdict)
				case verdict == "unbuildable":
					unrelated.invalid++
				case verdict == "panic":
					panics = append(panics, "unrelated "+name)
				default:
					unrelated.clean++
				}
			}
		}
	}
	t.Logf("tainted %+v", tainted)
	t.Logf("unrelated %+v", unrelated)
	// The pinned evaluator's Expr panics on {x: "v"}[y] inside a for. Production
	// recovers that into a refusal. Nothing else may panic.
	sort.Strings(panics)
	for _, entry := range panics {
		if !strings.HasPrefix(entry, "unrelated ") || !strings.HasSuffix(entry, "/for_once/index_value") {
			t.Errorf("PANIC %s", entry)
		}
	}
	if tainted.clean > 0 || tainted.detected < 700 || unrelated.clean < 1000 {
		t.Errorf("the generator stopped exercising analysis: tainted %+v, unrelated %+v", tainted, unrelated)
	}
	sort.Strings(escapes)
	sort.Strings(falseRejections)
	for _, entry := range escapes {
		t.Errorf("ESCAPE %s", entry)
	}
	for _, entry := range falseRejections {
		t.Errorf("FALSE-REJECTION %s", entry)
	}
}
