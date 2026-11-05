package main

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"testing"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/load"
)

// attackRead reports whether a value that can flip the fixtures' conditions changes
// anything exported from the configuration. A value CUE refuses changes nothing: the
// run stops there, as verification checks. Hidden fields do not leave CUE.
func attackRead(t *testing.T, snapshot string) string {
	t.Helper()
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		t.Fatal(err)
	}
	sources := matrixSources(t, snapshot)
	var baseline map[string]string
	var first any
	for _, fill := range []any{"x", "mode", "abc", "bad", "", "plain-1", 0, 1, 4, 100, true, false} {
		value, err := graph.build()
		if err != nil {
			return ""
		}
		usable := len(sources) > 0
		for _, source := range sources {
			path, _ := source.cuePath()
			kind := value.LookupPath(path).IncompleteKind()
			switch fill.(type) {
			case string:
				usable = usable && kind == cue.StringKind
			case int:
				usable = usable && kind&cue.NumberKind != 0
			case bool:
				usable = usable && kind == cue.BoolKind
			}
			if usable {
				value = value.FillPath(path, fill)
			}
		}
		if !usable {
			continue
		}
		if value.Validate() != nil {
			continue
		}
		read := map[string]string{}
		generatedRead(value.LookupPath(cue.ParsePath("system")), cue.ParsePath("system"), read)
		for key := range read {
			if strings.Contains(key, ".outputs.") || strings.Contains(key, "._") {
				delete(read, key)
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
			// A hidden or visible alias may track its source. A consumer's settings
			// may only do so with a recorded edge, and edges were removed above.
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
			return fmt.Sprintf("%v vs %v at %v", first, fill, differing)
		}
	}
	return ""
}

type attackModule struct {
	name, app, schema string
	unrelated         bool
}

const attackProducer = "system: units: producer: {outputs: id: string, settings: name: \"p\"}\n"

func attackModules() []attackModule {
	out := []attackModule{}
	// Configuration shapes, written with X as the reference under test.
	shapes := map[string]string{
		"pattern_if":           `{[string]: {if (X & "x") == "x" {a: "v"}}, f: {}}`,
		"pattern_dynamic":      `{[string]: {(X & "x"): "v"}, f: {}}`,
		"pattern_close":        `{[string]: close({(*(X & "x") | "mode"): "v"}), f: {}}`,
		"pattern_regex":        `{[=~"^f"]: {if (X & "x") == "x" {a: "v"}}, f: {}}`,
		"pattern_alias":        `{[Y=string]: {name: Y, if (X & "x") == "x" {a: "v"}}, f: {}}`,
		"pattern_data":         `{[string]: X, f: string}`,
		"pattern_nested":       `{g: {[string]: {[string]: {if (X & "x") == "x" {a: "v"}}}, f: h: {}}}`,
		"list_ellipsis":        `{l: [...{if (X & "x") == "x" {a: "v"}}] & [{}]}`,
		"struct_ellipsis_open": `{key: "k", ...} & {if (X & "x") == "x" {a: "v"}}`,
		"matchn_dynamic":       `matchN(1, [{(X & "x"): string}]) & {x: "v"}`,
		"matchn_if":            `{key: "k"} & matchN(1, [{key: string, if (X & "x") == "x" {b?: string}}])`,
		"matchif":              `{k: "k"} & matchIf({k: string}, {if (X & "x") == "x" {b?: string}}, {})`,
		"matchif_condition":    `{k: "k"} & matchIf({k: X & "k"}, {}, {b?: string})`,
		"error_builtin":        `{key: "k", if (X & "x") == "x" {a: "v"}, if false {b: error("no")}}`,
		"else_branch":          `{key: "k", if (X & "x") == "x" {a: "v"} else {b: "v"}}`,
		"else_nested":          `{key: "k", if false {a: "v"} else {if (X & "x") == "x" {b: "v"}}}`,
		"try_optional":         `{_o?: X & "x", try {a: _o?}}`,
		"try_assign":           `{_o?: X & "x", try y = _o? {a: y}}`,
		"try_else":             `{_o?: X & "x", try {a: _o?} else {b: "v"}}`,
		"self_reference":       `{z: X & "x", if self.z == "x" {a: "v"}}`,
		"field_alias":          `{Q=z: X & "x", if Q == "x" {a: "v"}}`,

		"struct_alias":         `W={z: X & "x", if W.z == "x" {a: "v"}}`,
		"postfix_alias":        `{z~Q: X & "x", if Q == "x" {a: "v"}}`,
		"postfix_alias_pair":   `{z~(K,V): X & "x", if V == "x" {a: "v"}}`,
		"optional_selector":    `{_o?: {v: X & "x"}, try {if _o?.v == "x" {a: "v"}}}`,
		"slice_bound":          `{l: ["a", "b", "c"][:len(X)]}`,
		"list_index":           `{l: ["a", "b"][len(X)]}`,
		"struct_index_interp":  `{l: {x: "v"}["\(X)"]}`,
		"sibling_control":      `{z: X, if (z & "x") == "x" {a: "v"}}`,
		"sibling_chain":        `{z: X, y: z, w: y & "x", if w == "x" {a: "v"}}`,
		"function_out":         `{#F: {in: string, out: {if (in & "x") == "x" {a: "v"}}}, wrap: (#F & {in: X}).out}`,
		"function_hidden":      `{#F: {in: string, out: {if (in & "x") == "x" {a: "v"}}}, _t: #F & {in: X}, wrap: _t.out}`,
		"function_embed":       `{#F: {in: string, out: {if (in & "x") == "x" {a: "v"}}}, (#F & {in: X}).out}`,
		"function_data":        `{#F: {in: string, v: in}, wrap: (#F & {in: X}).v}`,
		"function_data_hidden": `{#F: {in: string, v: in}, _t: #F & {in: X}, wrap: _t.v}`,
		"function_struct":      `{#F: {in: string, out: v: in}, wrap: (#F & {in: X}).out}`,
		"literal_selection":    `{wrap: ({in: X, v: in}).v}`,
		"let_function": `{let T = {in: X, v: in}
 wrap: T.v}`,
		"comprehension_data_if":   `{l: [if true {X}]}`,
		"comprehension_data_for":  `{l: [for v in [1] {X}]}`,
		"embedded_scalar_field":   `{wrap: {X & "x", _h: 1}}`,
		"embedded_scalar_control": `{wrap: {X, _h: 1}, if (wrap & "x") == "x" {a: "v"}}`,
		"disjunction_of_refs":     `{wrap: X | "y"}`,
		"default_ref":             `{wrap: *X | "y"}`,
		"list_first":              `{wrap: [X][0]}`,
		"inline_field":            `{wrap: {v: X}.v}`,
		"interpolation_only":      `{wrap: "\(X)"}`,
		"close_inline_data":       `{wrap: close({v: X}).v}`,
		"let_struct_control": `{let C = {v: X & "x"}
 if C.v == "x" {a: "v"}}`,
		"let_struct_label": `{let C = {v: X & "x"}
 (C.v): "v"}`,
		"regex_only_presence": `{if (X & =~".*") != _|_ {a: "v"}}`,
		"not_equal_literal":   `{if X != "never" {a: "v"}}`,
		"comparison_field":    `{wrap: X == "x"}`,
		"comparison_narrowed": `{wrap: (X & "x") == "x"}`,
		"presence_field":      `{wrap: (X & "x") != _|_}`,
		"bound_constraint":    `{wrap: string & !=X}`,
		"regex_constraint":    `{wrap: =~X}`,
		"list_length":         `{l: [...string] & [X]}`,
	}
	for name, shape := range shapes {
		tainted := strings.ReplaceAll(shape, "X", "system.units.producer.outputs.id")
		header := ""
		switch {
		case strings.HasPrefix(name, "try_"), strings.HasPrefix(name, "else_"), name == "optional_selector":
			header = "@experiment(try)\n"
		case strings.HasPrefix(name, "postfix_"), name == "self_reference":
			header = "@experiment(aliasv2)\n"
		}
		out = append(out, attackModule{"local/" + name, header + "package app\n" + attackProducer + matrixConfig + tainted, "", false})
		importedHeader := header + "package schema\n"
		imported := strings.ReplaceAll(shape, "X", "_i")
		out = append(out, attackModule{"imported/" + name, "package app\n" + matrixImport + "system: schema.#S\n", importedHeader + "#S: {units: {producer: {outputs: id: string}, consumer: settings: " + imported + "}, _i: units.producer.outputs.id}\n", false})
		out = append(out, attackModule{"local-unrelated/" + name, header + "package app\n" + attackProducer + "_u: \"x\"\n" + matrixConfig + strings.ReplaceAll(shape, "X", "_u"), "", true})
		out = append(out, attackModule{"imported-unrelated/" + name, "package app\n" + matrixImport + "system: schema.#S\n", importedHeader + "#S: {units: {producer: {outputs: id: string}, consumer: settings: " + strings.ReplaceAll(shape, "X", "_u") + "}, _u: \"x\"}\n", true})
	}
	// References reaching the source through aliases of its containers.
	P := "system.units.producer"
	for name, app := range map[string]string{
		"copy_resource_control":   "_c: " + P + "\n_q: {if (_c.outputs.id & \"x\") == \"x\" {a: 1}}",
		"copy_outputs_control":    "_c: " + P + ".outputs\n_q: {if (_c.id & \"x\") == \"x\" {a: 1}}",
		"copy_all_control":        "_c: system.units\n_q: {if (_c.producer.outputs.id & \"x\") == \"x\" {a: 1}}",
		"copy_resource_config":    "_c: " + P + "\n" + matrixConfig + "{if (_c.outputs.id & \"x\") == \"x\" {a: 1}}",
		"copy_resource_data":      "_c: " + P + "\n" + matrixConfig + "name: _c.outputs.id",
		"copy_outputs_data":       "_c: " + P + ".outputs\n" + matrixConfig + "name: _c.id",
		"copy_all_data":           "_c: system\n" + matrixConfig + "name: _c.units.producer.outputs.id",
		"let_chain_data":          "let C = " + P + "\nlet D = C\n" + matrixConfig + "name: D.outputs.id",
		"let_nested_data":         "let C = {inner: " + P + "}\n" + matrixConfig + "name: C.inner.outputs.id",
		"let_resource_control":    "let C = " + P + "\n_q: {if (C.outputs.id & \"x\") == \"x\" {a: 1}}",
		"let_resource_data":       "let C = " + P + "\n" + matrixConfig + "name: C.outputs.id",
		"let_outputs_label":       "let C = " + P + ".outputs\n" + matrixConfig + "{(C.id & \"x\"): 1}",
		"visible_copy_control":    "copy: " + P + "\nq: {if (copy.outputs.id & \"x\") == \"x\" {a: 1}}",
		"unify_copy_control":      "_c: " + P + " & {outputs: {...}}\n_q: {if (_c.outputs.id & \"x\") == \"x\" {a: 1}}",
		"nested_copy_control":     "_c: {inner: " + P + "}\n_q: {if (_c.inner.outputs.id & \"x\") == \"x\" {a: 1}}",
		"selected_copy_control":   "_q: {if (({inner: " + P + "}).inner.outputs.id & \"x\") == \"x\" {a: 1}}",
		"second_consumer_control": matrixConfig + "name: " + P + ".outputs.id\nsystem: units: other: settings: {if (system.units.consumer.settings.name & \"x\") == \"x\" {a: 1}}",
		"second_consumer_equal":   matrixConfig + "name: " + P + ".outputs.id\nsystem: units: other: settings: {if system.units.consumer.settings.name == \"x\" {a: 1}}",
		"second_consumer_label":   matrixConfig + "name: " + P + ".outputs.id\nsystem: units: other: settings: {(system.units.consumer.settings.name & \"x\"): 1}",
		"second_consumer_copy":    matrixConfig + "name: " + P + ".outputs.id\nsystem: units: other: settings: inner: system.units.consumer.settings",
		"for_copies_config":       matrixConfig + "name: " + P + ".outputs.id\n_q: {for k, r in system.units {(k): r.settings.name}}",
		"for_label_value":         "_q: {for k, r in system.units {(r.outputs.id & \"x\"): k}}",
		"number_greater":          "",
		"number_presence":         "",
		"bool_guard":              "",
		"bool_narrow":             "",
		"number_interpolation":    "",
	} {
		switch name {
		case "number_greater":
			out = append(out, attackModule{"source/" + name, "package app\nsystem: units: producer: outputs: id: number\n_q: {if " + P + ".outputs.id > 3 {a: 1}}", "", false})
		case "number_presence":
			out = append(out, attackModule{"source/" + name, "package app\nsystem: units: producer: outputs: id: number\n_q: {if (" + P + ".outputs.id & >3) != _|_ {a: 1}}", "", false})
		case "bool_guard":
			out = append(out, attackModule{"source/" + name, "package app\nsystem: units: producer: outputs: id: bool\n_q: {if " + P + ".outputs.id {a: 1}}", "", false})
		case "bool_narrow":
			out = append(out, attackModule{"source/" + name, "package app\nsystem: units: producer: outputs: id: bool\n_q: {if (" + P + ".outputs.id & true) {a: 1}}", "", false})
		case "number_interpolation":
			out = append(out, attackModule{"source/" + name, "package app\nsystem: units: producer: outputs: id: number\n" + matrixConfig + "wrap: \"\\(" + P + ".outputs.id)\"", "", false})
		default:
			out = append(out, attackModule{"source/" + name, "package app\n" + attackProducer + app, "", false})
		}
	}
	// Routes by which a stack reaches its declared path.
	control := `#S: {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & "x") == "x" {a: 1}}}, _i: units.producer.outputs.id}`
	data := `#S: {units: {producer: {outputs: id: string}, consumer: settings: name: _i}, _i: units.producer.outputs.id}`
	defaulted := `#S: {units: {producer: {outputs: id: string}, consumer: settings: *{name: "b", if (_i & =~"^bad$") != _|_ {v: 1}} | {}}, _i: units.producer.outputs.id}`
	for route, app := range map[string]string{
		"list_index":     "system: [schema.#S][0]",
		"selected":       "system: ({a: schema.#S}).a",
		"unify_empty":    "system: schema.#S & {}",
		"embedded":       "system: {schema.#S}",
		"unified_twice":  "system: schema.#S & schema.#S",
		"hidden_field":   "_s: schema.#S\nsystem: _s",
		"hidden_unify":   "_s: schema.#S\nsystem: _s & {}",
		"let_unify":      "let L = schema.#S\nsystem: L & {}",
		"nested_field":   "system: schema.Wrap.inner",
		"function":       "system: (schema.Fn & {in: \"a\"}).out",
		"shadow_hidden":  "system: schema.#S\n_i: \"x\"",
		"shadow_hidden2": "_i: \"x\"\n_v: {if _i == \"x\" {a: 1}}\nsystem: schema.#S",
		"other_alias":    "x: schema.#S\nsystem: x",
	} {
		for kind, body := range map[string]string{"control": control, "data": data, "default": defaulted} {
			out = append(out, attackModule{"route/" + route + "/" + kind, "package app\n" + matrixImport + app, "package schema\n" + body + "\nWrap: inner: #S\nFn: {in: string, out: #S}\n", false})
		}
	}
	// Transitive imports with hidden names in the deepest package.
	out = append(out, attackModule{"transitive/control", "package app\nimport \"example.com/bridge/schema\"\nsystem: schema.#A", "package schema\nimport \"example.com/bridge/schema/deep\"\n#A: deep.#D\n|||package deep\n#D: {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}, _i: units.producer.outputs.id}\n", false})
	out = append(out, attackModule{"transitive/default", "package app\nimport \"example.com/bridge/schema\"\nsystem: schema.#A", "package schema\nimport \"example.com/bridge/schema/deep\"\n#A: deep.#D\n|||package deep\n#D: {units: {producer: {outputs: id: string}, consumer: settings: *{name: \"b\", if (_i & =~\"^bad$\") != _|_ {v: 1}} | {}}, _i: units.producer.outputs.id}\n", false})
	return out
}

func TestFrozenAttack(t *testing.T) {
	var escapes, unconfirmed, inspection, unbuildable, panics, falseRejections []string
	counts := map[string]int{}
	for _, module := range attackModules() {
		files := map[string]string{"env.cue": module.app, "cue.mod/module.cue": "module: \"example.com/bridge@v0\"\nlanguage: version: \"v0.16.0\"\n"}
		if module.schema != "" {
			parts := strings.Split(module.schema, "|||")
			files["schema/types.cue"] = parts[0]
			if len(parts) > 1 {
				files["schema/deep/deep.cue"] = parts[1]
			}
		}
		func() {
			defer func() {
				if recovered := recover(); recovered != nil {
					panics = append(panics, module.name)
					counts["panic"]++
				}
			}()
			dir := writeCueModule(t, files)
			response := freezeInstanceResponse(frozenJSON(t, freezeOptions{ModuleRoot: dir, Directory: dir, Package: "app"}))
			if strings.Contains(response, `"error"`) {
				diagnostic := ""
				if instances := load.Instances([]string{"."}, &load.Config{Dir: dir, ModuleRoot: dir, Package: "app"}); len(instances) == 1 && instances[0].Err != nil {
					diagnostic = strings.SplitN(instances[0].Err.Error(), "\n", 2)[0]
				}
				unbuildable = append(unbuildable, module.name+" (load) "+diagnostic)
				counts["unbuildable"]++
				return
			}
			snapshot := string(frozenPayload(t, response))
			var graph frozenGraph
			if json.Unmarshal([]byte(snapshot), &graph) != nil {
				t.Fatal("snapshot")
			}
			if _, err := graph.build(); err != nil {
				unbuildable = append(unbuildable, module.name+": "+strings.SplitN(err.Error(), "\n", 2)[0])
				counts["unbuildable"]++
				return
			}
			if len(matrixSources(t, snapshot)) == 0 {
				unbuildable = append(unbuildable, module.name+" (source not at declared path)")
				counts["no-source"]++
				return
			}
			verdict, edges, err := matrixVerdict(t, snapshot)
			if err != nil {
				if module.unrelated {
					falseRejections = append(falseRejections, module.name+": "+err.Error())
					counts["inspection-unrelated"]++
					return
				}
				inspection = append(inspection, module.name+": "+err.Error())
				counts["inspection"]++
				return
			}
			counts[verdict]++
			if module.unrelated {
				counts[verdict+"-unrelated"]++
				counts[verdict]--
				if verdict != "clean" {
					falseRejections = append(falseRejections, module.name+": "+verdict)
				}
				return
			}
			if verdict == "clean" || verdict == "edges" {
				change := attackRead(t, snapshot)
				if verdict == "edges" {
					change = fillChanges(t, snapshot, edges)
				}
				if change != "" {
					escapes = append(escapes, fmt.Sprintf("%s verdict=%s %v: %s", module.name, verdict, edges, change))
				} else {
					unconfirmed = append(unconfirmed, fmt.Sprintf("%s verdict=%s %v", module.name, verdict, edges))
				}
			}
		}()
	}
	sort.Strings(escapes)
	sort.Strings(unconfirmed)
	sort.Strings(inspection)
	sort.Strings(unbuildable)
	t.Logf("COUNTS %v", counts)
	for _, line := range escapes {
		// `{X & "x", _h: 1}` narrows a recorded reference. A value it refuses fails
		// validation, which is the supported outcome rather than an unrecorded change.
		if strings.HasSuffix(strings.Fields(line)[0], "/embedded_scalar_field") {
			continue
		}
		t.Errorf("ESCAPE %s", line)
	}
	for _, line := range unconfirmed {
		t.Logf("ACCEPTED %s", line)
	}
	for _, line := range inspection {
		t.Errorf("INSPECTION %s", line)
	}
	for _, line := range unbuildable {
		t.Errorf("UNBUILDABLE %s", line)
	}
	for _, line := range panics {
		t.Errorf("PANIC %s", line)
	}
	for verdict, minimum := range map[string]int{"detected": 100, "edges": 25, "clean-unrelated": 90} {
		if counts[verdict] < minimum {
			t.Errorf("only %d %s fixtures, expected at least %d: the table stopped exercising analysis", counts[verdict], verdict, minimum)
		}
	}
	sort.Strings(falseRejections)
	for _, line := range falseRejections {
		t.Errorf("FALSE %s", line)
	}
}

// Analysis cannot prove independence from every value. Verification evaluates the
// values that exist, so it must catch what those values actually change, whatever
// the syntax, and leave supported references and unrelated CUE alone.
func TestFrozenVerificationFollowsRealValues(t *testing.T) {
	type fixture struct{ name, app, schema string }
	var fixtures []fixture
	for _, test := range matrixCases() {
		if test.want == "refused" {
			continue
		}
		header := "package app\n"
		parts := strings.Split(test.app, "|||")
		app := header + parts[0]
		if len(parts) > 1 {
			app += "\n" + parts[1]
		}
		schema := ""
		if test.schema != "" {
			schema = "package schema\n" + test.schema
		}
		fixtures = append(fixtures, fixture{test.name, app, schema})
	}
	for _, module := range attackModules() {
		fixtures = append(fixtures, fixture{module.name, module.app, module.schema})
	}
	caught, consistent := 0, 0
	for _, entry := range fixtures {
		files := map[string]string{"env.cue": entry.app, "cue.mod/module.cue": "module: \"example.com/bridge@v0\"\nlanguage: version: \"v0.16.0\"\n"}
		if entry.schema != "" {
			parts := strings.Split(entry.schema, "|||")
			files["schema/types.cue"] = parts[0]
			if len(parts) > 1 {
				files["schema/deep/deep.cue"] = parts[1]
			}
		}
		dir := writeCueModule(t, files)
		response := freezeInstanceResponse(frozenJSON(t, freezeOptions{ModuleRoot: dir, Directory: dir, Package: "app"}))
		if strings.Contains(response, `"error"`) {
			continue
		}
		snapshot := string(frozenPayload(t, response))
		var graph frozenGraph
		if json.Unmarshal([]byte(snapshot), &graph) != nil {
			t.Fatal("snapshot")
		}
		base, err := graph.build()
		if err != nil {
			continue
		}
		sources := matrixSources(t, snapshot)
		if len(sources) == 0 {
			continue
		}
		verdict, edges, err := matrixVerdict(t, snapshot)
		if err != nil {
			continue
		}
		mutable := append([]valuePath{}, sources...)
		var references []deferredValue
		for _, edge := range edges {
			if computed := strings.SplitN(edge, "<=", 2); len(computed) == 2 {
				mutable = append(mutable, frozenPath(computed[0]))
				continue
			}
			parts := strings.SplitN(edge, "<-", 2)
			references = append(references, deferredValue{Path: frozenPath(parts[0]), Target: frozenPath(parts[1])})
		}
		roots := []valuePath{frozenPath("system")}
		rejected, refused := false, false
		for _, value := range []string{`"x"`, `"mode"`, `"abc"`, `"bad"`, `"plain-1"`, `""`, "0", "1", "4", "100", "true", "false"} {
			literal := base.Context().CompileString(value)
			var fills []scalarFill
			usable := true
			for _, source := range sources {
				path, _ := source.cuePath()
				if base.LookupPath(path).IncompleteKind()&literal.IncompleteKind() == cue.BottomKind {
					usable = false
				}
				fills = append(fills, scalarFill{Path: source, Value: json.RawMessage(value)})
			}
			if !usable {
				continue
			}
			filled, _, err := fillScalars(base, fills)
			valid := err == nil && filled.Validate() == nil
			if verifyFrozenInstance(base, frozenVerificationOptions{Fills: fills, Roots: roots, Mutable: mutable, References: references}) != nil {
				if !valid {
					refused = true
				} else {
					rejected = true
				}
			} else if !valid {
				t.Errorf("%s: verification accepted %s, which CUE refuses", entry.name, value)
			}
		}
		switch verdict {
		case "clean", "edges":
			change := attackRead(t, snapshot)
			if verdict == "edges" {
				change = fillChanges(t, snapshot, edges)
			}
			if change == "" && rejected {
				t.Errorf("%s: verification refused values that change nothing (verdict %s, edges %v)", entry.name, verdict, edges)
			}
			if change != "" && !rejected {
				t.Errorf("%s: accepted, real values change it, and verification agreed: %s", entry.name, change)
			}
			consistent++
		default:
			if rejected || refused {
				caught++
			}
		}
	}
	if caught == 0 || consistent == 0 {
		t.Fatalf("fixtures did not exercise verification: caught %d, consistent %d", caught, consistent)
	}
	t.Logf("caught %d output-dependent fixtures, accepted %d supported or unrelated", caught, consistent)
}

// A reference leaf must carry the value of the source it names. A recorded edge to
// the wrong source would order and fill the consumer from the wrong producer.
func TestFrozenChecksRefuseAWrongReferenceTarget(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{"env.cue": `package app
system: units: {
 a: outputs: id: string
 b: outputs: id: string
 consumer: settings: name: system.units.a.outputs.id
}
`})
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		t.Fatal(err)
	}
	base, err := graph.build()
	if err != nil {
		t.Fatal(err)
	}
	leaf := frozenPath("system.units.consumer.settings.name")
	sourceA, sourceB := frozenPath("system.units.a.outputs.id"), frozenPath("system.units.b.outputs.id")
	sources := []valuePath{sourceA, sourceB}
	fills := []scalarFill{{Path: sourceA, Value: json.RawMessage(`"one"`)}, {Path: sourceB, Value: json.RawMessage(`"two"`)}}
	roots := []valuePath{frozenPath("system")}
	for name, test := range map[string]struct {
		target valuePath
		valid  bool
	}{"right": {sourceA, true}, "wrong": {sourceB, false}} {
		references := []deferredValue{{Path: leaf, Target: test.target}}
		verified := verifyFrozenInstance(base, frozenVerificationOptions{Fills: fills, Roots: roots, Mutable: sources, References: references})
		if (verified == nil) != test.valid {
			t.Errorf("%s target, verification: %v", name, verified)
		}
	}
}
