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

const (
	matrixSource = "system.units.producer.outputs.id"
	matrixBase   = "system: units: producer: {outputs: id: string, settings: name: \"p\"}\n"
	matrixImport = "import \"example.com/bridge/schema\"\n"
	matrixConfig = "system: units: consumer: settings: "
	matrixEdge   = "system.units.consumer.settings.name<-" + matrixSource
)

// matrixCase asserts the verdict, its exact edges, and for rejections that the
// dependency was detected: an inspection error must not pass as detection.
type matrixCase struct {
	name, app, schema string
	want              string // "clean", "edges", "detected" or "incomplete"
	edges             []string
}

func clean(name, app, schema string) matrixCase { return matrixCase{name, app, schema, "clean", nil} }

// Refused either by a dependency rule or because independence cannot be established.
func refused(name, app, schema string) matrixCase {
	return matrixCase{name, app, schema, "refused", nil}
}
func detected(name, app, schema string) matrixCase {
	return matrixCase{name, app, schema, "detected", nil}
}
func edges(name, app, schema string, expected ...string) matrixCase {
	return matrixCase{name, app, schema, "edges", expected}
}

// A check that nothing in the configuration reads is accepted, and real values are
// verified against it. Reading what it decides from the configuration is refused.
func unreadCheck(name, app, schema, field string) []matrixCase {
	return []matrixCase{
		clean(name, app, schema),
		detected(name+"_read", app+"\n"+matrixConfig+"read: "+field, schema),
	}
}

func stack(control string) string {
	return "#S: {units: {producer: {outputs: id: string}, consumer: settings: " + control + "}, _i: units.producer.outputs.id, _u: \"x\"}"
}

func matrixCases() []matrixCase {
	var checks []matrixCase
	check := func(name, app, schema, field string) matrixCase {
		both := unreadCheck(name, app, schema, field)
		checks = append(checks, both[1])
		return both[0]
	}
	source := matrixSource
	narrowed := "(" + source + " & \"x\")"
	deep := "{name: \"base\", if (" + source + " & =~\"^bad$\") != _|_ {value: \"c\"}}"
	for range 70 {
		deep = "{nested: " + deep + "}"
	}
	dynamic := "(*" + narrowed + " | \"mode\"): \"value\""
	cases := []matrixCase{
		// Valid CUE unrelated to runtime outputs.
		clean("issue_hidden_if", matrixBase+"_b: \"x\"\n_z: {if _b == \"x\" {v: 1}}", ""),
		clean("issue_hidden_optional", matrixBase+"_b?: string\n_z: {if _b == _|_ {v: 1}}", ""),
		clean("issue_hidden_for", matrixBase+"_xs: [\"a\"]\n_z: {for x in _xs {(x): 1}}", ""),
		clean("issue_list_comprehension", matrixBase+"_b?: string\nconfig: v: [if _b != _|_ {_b}, \"0\"][0]", ""),
		clean("imported_hidden", matrixImport+matrixBase+"x: schema.#D", "#D: {_b: \"x\", z: {if _b == \"x\" {v: 1}}}"),
		clean("imported_hidden_in_config", matrixImport+"system: schema.#S", "#S: {units: {producer: {outputs: id: string}, consumer: settings: {_b: \"x\", if _b == \"x\" {v: 1}}}}"),
		clean("imported_constant", matrixImport+matrixBase+"_v: schema.C\n_z: {if _v == \"x\" {a: 1}}", "C: \"x\""),
		clean("imported_optional_hidden", matrixImport+matrixBase+"x: schema.#O", "#O: {_o?: string, z: {if _o == _|_ {v: 1}}}"),
		clean("imported_hidden_for", matrixImport+matrixBase+"x: schema.#F", "#F: {_xs: [\"a\"], z: {for v in _xs {(v): 1}}}"),
		clean("imported_list_for", matrixImport+matrixBase+"_z: {for v in schema.L {(v): 1}}", "L: [\"a\"]"),
		clean("for_keys", matrixBase+"_n: {for k, _ in system.units {(k): true}}", ""),
		clean("for_unrelated_field", matrixBase+"system: units: producer: type: \"t\"\n_q: {for k, r in system.units {if r.type == \"t\" {(k): 1}}}", ""),
		clean("shadow", matrixBase+"_s: {id: \"lit\", z: {if id == \"lit\" {a: 1}}}\nid: \"other\"", ""),
		clean("multi_file_hidden", matrixBase+"_z: {if _b == \"x\" {v: 1}}\n|||_b: \"x\"", ""),
		clean("builtin", "import \"strings\"\n"+matrixBase+"_x: \"abc\"\n_z: {if strings.HasPrefix(_x, \"a\") {v: 1}}", ""),
		clean("root_embed_control", matrixImport+"schema.#T", "#T: {system: units: {producer: {outputs: id: string}, consumer: settings: {if (_u & \"x\") == \"x\" {a: 1}}}\n _u: \"x\"}"),
		clean("root_embed_presence", matrixImport+"schema.#T", "#T: {_u?: string, system: units: {producer: {outputs: id: string}, consumer: settings: {name: \"n\", if _u != _|_ {a: _u}}}}"),
		clean("default_local", matrixBase+"_u: \"bb\"\n"+matrixConfig+"*{name: \"b\", if (_u & =~\"^b\") != _|_ {v: 1}} | {}", ""),
		clean("default_imported", matrixImport+"system: schema.#S", stack("*{name: \"b\", if (_u & =~\"^x\") != _|_ {v: 1}} | {}")),
		clean("default_let_imported", matrixImport+"system: schema.#S", stack("*{let L = _u\n if L == \"x\" {a: 1}} | {}")),
		clean("close_unrelated_dynamic", matrixImport+"system: schema.#S", stack("close({key: \"k\", (*_u | \"mode\"): \"value\"})")),
		clean("close_local_unrelated_dynamic", matrixBase+"_u: \"x\"\n"+matrixConfig+"close({key: \"k\", (_u): \"value\"})", ""),
		clean("let_holds_stack", "let S = {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}, _i: \"x\"}\nsystem: S", ""),
		clean("disjunct_imported_stack", matrixImport+"system: *schema.#G | schema.#G", "#G: {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}, _i: \"x\"}"),
		clean("disjunct_struct_stack", matrixImport+"system: *{schema.#G} | {schema.#G}", "#G: {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}, _i: \"x\"}"),
		clean("hidden_definition", matrixImport+"system: schema.#S", "_#D: {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}, _i: \"x\"}\n#S: {_#D}"),

		clean("for_key_value", matrixBase+"_m: {a: 1}\nsystem: units: producer: settings: tags: {for k, v in _m {(k): v}}", ""),
		clean("list_comprehension_field", matrixBase+"_xs: [\"a\"]\nsystem: units: producer: settings: l: [for y in _xs {y}]", ""),
		clean("let_computed_unify", matrixBase+"#D: {a: string, b: string}\nlet C = #D & {b: \"x\"}\nsystem: units: producer: settings: n: C.b", ""),
		clean("let_literal_unify", matrixBase+"let C = {a: 1} & {b: 2}\nsystem: units: producer: settings: n: C.a", ""),
		clean("list_concat_comprehension", "import \"list\"\n"+matrixBase+"_a: [\"a\"]\nsystem: units: producer: settings: l: list.Concat([[for y in _a {y}], [\"b\"]])", ""),
		clean("validator_unrelated", matrixBase+"_u: \"x\"\nsystem: units: producer: settings: k: matchN(1, [_u]) & \"x\"", ""),
		clean("json_unrelated", "import \"encoding/json\"\n"+matrixBase+"_u: \"x\"\nsystem: units: producer: settings: doc: json.Marshal({a: *(_u & \"x\") | \"z\"})", ""),

		clean("default_over_absent_optional", matrixBase+"_x: {a?: string}\nsystem: units: consumer: settings: name: *_x.a | \"fallback\"", ""),
		clean("imported_default_over_absent_optional", matrixImport+"system: schema.#O", "#O: {units: {producer: {outputs: id: string}, consumer: settings: name: *_x.a | \"fallback\"}, _x: {a?: string}}"),
		clean("presence_of_absent_field", matrixBase+"_q: {if system.units.nope == _|_ {a: 1}}", ""),

		// Supported references, with their exact edges.
		edges("constraint_reads_constant", matrixBase+"_max: 10\nsystem: units: producer: outputs: n: number\n"+matrixConfig+"n: system.units.producer.outputs.n & <=_max", "", "system.units.consumer.settings.n<-system.units.producer.outputs.n"),
		edges("constraint_reads_constant_pattern", matrixBase+"_p: \"^.\"\n"+matrixConfig+"name: "+source+" & =~_p", "", matrixEdge),
		edges("definition_constraint", matrixBase+"#Id: string\n#C: {name: #Id}\n"+matrixConfig+"#C & {name: "+source+"}", "", matrixEdge),
		edges("definition_conjunct", matrixBase+"#Id: string\n"+matrixConfig+"name: "+source+" & #Id", "", matrixEdge),
		edges("hidden_constraint", matrixBase+"_h: string\n"+matrixConfig+"name: "+source+" & _h", "", matrixEdge),
		edges("let_selection", matrixBase+"let C = system.units.producer\n"+matrixConfig+"name: C.outputs.id", "", matrixEdge),
		edges("let_chain_selection", matrixBase+"let C = system.units.producer\nlet D = C\n"+matrixConfig+"name: D.outputs.id", "", matrixEdge),
		edges("let_literal_selection", matrixBase+"let C = {inner: system.units.producer}\n"+matrixConfig+"name: C.inner.outputs.id", "", matrixEdge),
		edges("direct", matrixBase+matrixConfig+"name: "+source, "", matrixEdge),
		edges("nested", matrixBase+matrixConfig+"a: b: [{t: "+source+"}]", "", "system.units.consumer.settings.a.b[0].t<-"+source),
		edges("let_value", matrixBase+"let L = "+source+"\n"+matrixConfig+"name: L", "", matrixEdge),
		edges("imported_alias", matrixImport+"system: schema.#S", stack("name: _i"), matrixEdge),
		edges("imported_alias_constrained", matrixImport+"system: schema.#S", stack("name: _i & =~\"^p\""), matrixEdge),
		edges("imported_alias_unified", matrixImport+"system: schema.#S & {units: extra: outputs: x: string}", "#S: {units: {producer: {outputs: id: string}, consumer: settings: name: _i, ...}, _i: units.producer.outputs.id}", matrixEdge),
		edges("root_embed_alias", matrixImport+"schema.#T", "#T: {system: units: {producer: {outputs: id: string}, consumer: settings: name: _i}\n _i: system.units.producer.outputs.id}", matrixEdge),
		edges("two_booleans", "system: units: {a: outputs: flag: bool, z: outputs: flag: bool}\n"+matrixConfig+"{from_a: system.units.a.outputs.flag, from_z: system.units.z.outputs.flag, literal: true}", "",
			"system.units.consumer.settings.from_a<-system.units.a.outputs.flag",
			"system.units.consumer.settings.from_z<-system.units.z.outputs.flag"),
		edges("lookup_to_unit", "system: {lookups: lookup: outputs: value: string, units: consumer: settings: name: system.lookups.lookup.outputs.value}", "",
			"system.units.consumer.settings.name<-system.lookups.lookup.outputs.value"),
		edges("unit_to_lookup", matrixBase+"system: lookups: lookup: settings: name: "+source, "",
			"system.lookups.lookup.settings.name<-"+source),
		edges("close_with_reference", matrixBase+matrixConfig+"close({name: "+source+"})", "", matrixEdge),

		// Output-dependent structure.
		detected("imported_conditional", matrixImport+"system: schema.#S", stack("{if (_i & \"x\") == \"x\" {name: \"c\"}}")),
		detected("imported_conditional_unified", matrixImport+"system: schema.#S & {units: extra: outputs: x: string}", "#S: {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {name: \"c\"}}, ...}, _i: units.producer.outputs.id}"),
		detected("imported_let", matrixImport+"system: schema.#S", "#S: {units: producer: outputs: id: string\n let L = units.producer.outputs.id\n units: consumer: settings: {if (L & \"x\") == \"x\" {a: 1}}}"),
		detected("imported_plain_equal", matrixImport+"system: schema.#S", stack("{if _i == \"x\" {a: 1}}")),
		clean("imported_hidden_control", matrixImport+"system: schema.#D\n_i: \"x\"", "#D: {units: producer: outputs: id: string, _i: units.producer.outputs.id, _z: {if (_i & \"x\") == \"x\" {a: 1}}}"),
		detected("imported_hidden_control_read", matrixImport+"system: schema.#D\n_i: \"x\"", "#D: {units: producer: outputs: id: string, _i: units.producer.outputs.id, _z: {if (_i & \"x\") == \"x\" {a: 1}}, units: consumer: settings: read: _z}"),
		clean("caller_argument", matrixImport+matrixBase+"_q: schema.#G & {arg: "+source+" & \"x\"}", "#G: {arg: string, z: {if arg == \"x\" {a: 1}}}"),
		detected("caller_argument_read", matrixImport+matrixBase+"_q: schema.#G & {arg: "+source+" & \"x\"}\n"+matrixConfig+"read: _q.z", "#G: {arg: string, z: {if arg == \"x\" {a: 1}}}"),
		detected("definition_chain", matrixImport+"system: schema.#A", "#A: #S & {z: 1}\n"+"#S: {z: int, units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}, _i: units.producer.outputs.id}"),
		detected("definition_embedded", matrixImport+"system: schema.#E", "#E: {#S, ...}\n"+stack("{if (_i & \"x\") == \"x\" {a: 1}}")),
		detected("root_embed", matrixImport+"schema.#T", "#T: {system: units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}\n _i: system.units.producer.outputs.id}"),
		detected("package_value", matrixImport+"system: schema.Stack", "Stack: {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}, _i: units.producer.outputs.id}"),
		detected("nested_definition", matrixImport+"system: schema.#S", "#S: {units: {producer: {outputs: id: string}, consumer: settings: #In & {arg: units.producer.outputs.id}}}\n#In: {arg: string, if (arg & \"x\") == \"x\" {v: 1}}"),
		detected("let_holds_stack_control", "let S = {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}, _i: units.producer.outputs.id}\nsystem: S", ""),
		detected("disjunct_stack_control", matrixImport+"system: *schema.#S | schema.#S", stack("{if (_i & \"x\") == \"x\" {a: 1}}")),
		detected("disjunct_struct_stack_control", matrixImport+"system: *{schema.#S} | {schema.#S}", stack("{if (_i & \"x\") == \"x\" {a: 1}}")),
		detected("hidden_definition_control", matrixImport+"system: schema.#S", "_#D: {units: {producer: {outputs: id: string}, consumer: settings: {if (_i & \"x\") == \"x\" {a: 1}}}, _i: units.producer.outputs.id}\n#S: {_#D}"),
		detected("default_imported_control", matrixImport+"system: schema.#S", stack("*{name: \"b\", if (_i & =~\"^bad$\") != _|_ {v: 1}} | {}")),
		detected("default_let_control", matrixImport+"system: schema.#S", stack("*{let L = _i\n if (L & \"x\") == \"x\" {a: 1}} | {}")),
		detected("local_equal", matrixBase+matrixConfig+"{if "+source+" == \"x\" {a: 1}}", ""),
		check("hidden_control", matrixBase+"_q: {if "+narrowed+" == \"x\" {a: 1}}", "", "_q"),
		detected("let_control", matrixBase+"let L = "+source+"\n"+matrixConfig+"{if (L & \"x\") == \"x\" {a: 1}}", ""),
		check("let_chain", matrixBase+"let L = "+source+"\nlet M = L & \"x\"\n_z: {if M == \"x\" {a: 1}}", "", "_z"),
		detected("let_presence", matrixBase+"let M = "+source+" & =~\"^bad$\"\n"+matrixConfig+"{name: \"b\", if M != _|_ {value: \"c\"}}", ""),
		detected("regex_presence", matrixBase+matrixConfig+"{if ("+source+" & =~\"^a\") != _|_ {a: 1}}", ""),
		detected("default_marker", matrixBase+matrixConfig+"*{name: \"base\", if ("+source+" & =~\"^bad$\") != _|_ {value: \"c\"}} | {}", ""),
		detected("default_second_arm", matrixBase+matrixConfig+"{} | *{if ("+source+" & =~\"^bad$\") != _|_ {value: \"c\"}}", ""),
		detected("default_unselected_arm", matrixBase+matrixConfig+"*{c: 1} | {a: {("+narrowed+"): 1}}", ""),
		detected("parenthesised_and", matrixBase+matrixConfig+"({name: \"b\"} & ({if "+narrowed+" == \"x\" {v: 1}}))", ""),
		detected("deep_nesting", matrixBase+matrixConfig+deep, ""),
		check("index", matrixBase+"_m: {x: 1}\n_q: _m["+source+"]", "", "_q"),
		detected("dynamic_label", matrixBase+matrixConfig+"{"+narrowed+": 1}", ""),
		detected("pattern_label", matrixBase+matrixConfig+"{["+narrowed+"]: int, x: 1}", ""),
		check("for_value_control", matrixBase+"_q: {for k, r in system.units {if (r.outputs.id & \"x\") == \"x\" {(k): 1}}}", "", "_q"),
		check("for_over_output", matrixBase+"_q: {for x in ["+narrowed+"] {(x): 1}}", "", "_q"),
		edges("interpolation", matrixBase+matrixConfig+"n: \"a-\\("+source+")\"", "", "system.units.consumer.settings.n<="+source),
		edges("arithmetic", "system: units: producer: outputs: n: number\n"+matrixConfig+"n: system.units.producer.outputs.n + 1", "", "system.units.consumer.settings.n<=system.units.producer.outputs.n"),
		check("inline_struct", matrixBase+"_q: {if ({v: "+source+"}).v == \"x\" {a: 1}}", "", "_q"),
		check("true_guard", matrixBase+"_q: {if true {if "+source+" == \"x\" {a: 1}}}", "", "_q"),
		clean("false_guard", matrixBase+"_q: {if false {if "+source+" == \"x\" {a: 1}}}", ""),
		clean("untracked_predicate", matrixBase+"_chk: "+source+" & =~\"^a\"", ""),
		edges("untracked_predicate_read", matrixBase+"_chk: "+source+" & =~\"^a\"\n"+matrixConfig+"name: _chk", "", matrixEdge),
		detected("builtin_control", "import \"strings\"\n"+matrixBase+matrixConfig+"{if strings.HasPrefix("+source+", \"a\") {v: 1}}", ""),
		detected("default_data_arm", matrixBase+matrixConfig+"*{name: "+source+"} | {name: \"x\"}", ""),
		// The call family hides syntax from the path-driven structural walk.
		detected("close_dynamic_default", matrixBase+matrixConfig+"close({key: \"consumer\", "+dynamic+"})", ""),
		detected("close_dynamic_plain", matrixBase+matrixConfig+"close({("+source+"): \"value\"})", ""),
		detected("close_interpolated_label", matrixBase+matrixConfig+"close({\"\\("+narrowed+")\": \"value\"})", ""),
		detected("close_pattern", matrixBase+matrixConfig+"close({["+narrowed+"]: string, x: \"v\"})", ""),
		detected("close_comprehension", matrixBase+matrixConfig+"close({if "+narrowed+" == \"x\" {a: 1}})", ""),
		detected("close_nested", matrixBase+matrixConfig+"close(close({"+dynamic+"}))", ""),
		detected("close_in_conjunction", matrixBase+matrixConfig+"{key: \"k\"} & close({key: string, "+dynamic+"})", ""),
		detected("close_in_field", matrixBase+matrixConfig+"inner: close({"+dynamic+"})", ""),
		detected("close_in_list", matrixBase+matrixConfig+"items: [close({"+dynamic+"})]", ""),
		detected("close_through_let", matrixBase+"let C = close({"+dynamic+"})\n"+matrixConfig+"C", ""),
		detected("close_through_hidden", matrixBase+"_c: close({"+dynamic+"})\n"+matrixConfig+"_c", ""),
		detected("close_imported", matrixImport+"system: schema.#S", stack("close({key: \"k\", (*(_i & \"x\") | \"mode\"): \"value\"})")),
		detected("close_imported_definition", matrixImport+matrixBase+matrixConfig+"schema.#C & {arg: "+source+"}", "#C: close({arg: string, (*(arg & \"x\") | \"mode\"): \"value\"})"),
		detected("and_builtin", matrixBase+matrixConfig+"and([{"+dynamic+"}, {key: \"k\"}])", ""),
		detected("or_builtin", matrixBase+matrixConfig+"or([{"+dynamic+"}])", ""),
		detected("selector_of_struct", matrixBase+matrixConfig+"({inner: {"+dynamic+"}}).inner", ""),
		detected("index_of_list", matrixBase+matrixConfig+"[{"+dynamic+"}][0]", ""),
		detected("embedded_struct", matrixBase+matrixConfig+"{{"+dynamic+"}}", ""),
		detected("lookup_close", "system: {lookups: lookup: outputs: value: string, units: consumer: settings: close({(*(system.lookups.lookup.outputs.value & \"x\") | \"mode\"): \"value\"})}", ""),
		detected("unit_to_lookup_close", matrixBase+"system: lookups: lookup: settings: close({"+dynamic+"})", ""),
		detected("default_arm_field", matrixBase+matrixConfig+"*{w: "+source+" & \"x\"} | {w: \"z\"}", ""),
		detected("default_scalar", matrixBase+matrixConfig+"w: *("+source+" & \"x\") | \"z\"", ""),
		detected("imported_default_arm", matrixImport+"system: schema.#S", stack("*{w: _i & \"x\"} | {w: \"z\"}")),
		detected("imported_default_scalar_shadowed", matrixImport+"system: schema.#S\n_i: \"safe\"", stack("w: *(_i & \"x\") | \"z\"")),
		refused("alias_container_default", matrixBase+"_s: {a: "+source+"}\n"+matrixConfig+"*({a: \"y\"} & _s) | {a: \"q\"}", ""),
		refused("alias_list_default", matrixBase+"_l: ["+source+"]\n"+matrixConfig+"w: *([\"y\"] & _l)[0] | \"q\"", ""),
		detected("json_builtin", "import \"encoding/json\"\n"+matrixBase+matrixConfig+"doc: json.Marshal({a: *("+source+" & \"x\") | \"z\"})", ""),
		detected("validator_argument", matrixBase+"_v: matchN(1, ["+source+"])\n"+matrixConfig+"name: *(\"y\" & _v) | \"q\"", ""),
		detected("let_default_leaf", matrixBase+"let T = {v: *("+source+" & \"x\") | \"z\"}\n"+matrixConfig+"name: T.v", ""),
		detected("let_interpolated_default", matrixBase+"let T = {v: \"p-\\(*("+source+" & \"x\") | \"z\")\"}\n"+matrixConfig+"name: T.v", ""),
		edges("let_struct_edge", matrixBase+"let T = {cfg: {name: "+source+", extra: 1}}\n"+matrixConfig+"T.cfg", "", matrixEdge),
		detected("copy_embedded", matrixBase+"_c: {system.units.producer}\n"+matrixConfig+"*{w: _c.outputs.id & \"x\"} | {w: \"z\"}", ""),
		detected("copy_closed", matrixBase+"_c: close(system.units.producer)\n"+matrixConfig+"*{w: _c.outputs.id & \"x\"} | {w: \"z\"}", ""),
		detected("copy_default", matrixBase+"_c: *system.units.producer | {}\n"+matrixConfig+"*{w: _c.outputs.id & \"x\"} | {w: \"z\"}", ""),
		detected("copy_or", matrixBase+"_c: or([system.units.producer])\n"+matrixConfig+"*{w: _c.outputs.id & \"x\"} | {w: \"z\"}", ""),
		edges("two_source_unification", "system: units: {a: outputs: id: string, b: outputs: id: string}\n_c: system.units.a & system.units.b\n"+matrixConfig+"name: _c.outputs.id", "", "system.units.consumer.settings.name<=system.units.a.outputs.id", "system.units.consumer.settings.name<=system.units.b.outputs.id"),
		detected("json_presence", "import \"encoding/json\"\n"+matrixBase+matrixConfig+"doc: json.Marshal({a: ("+source+" & =~\"^x\") != _|_})", ""),
		detected("imported_json_presence", matrixImport+matrixBase+matrixConfig+"schema.#J & {arg: "+source+"}", "import \"encoding/json\"\n#J: {arg: string, doc: json.Marshal({a: (arg & =~\"^x\") != _|_})}"),
		detected("validator_struct_argument", matrixBase+"_v: matchN(1, [{k: string, _x: "+source+" & !=\"bad\"}])\n"+matrixConfig+"*({k: \"a\"} & _v) | {k: \"q\"}", ""),
		detected("constrained_source_keeps_sampling", "import \"encoding/json\"\n"+matrixBase+matrixConfig+"{prefix: "+source+" & =~\"^[a-z-]+$\", doc: json.Marshal({a: *("+source+" & \"x\") | \"z\"})}", ""),
		detected("let_selection_with_default", matrixBase+"let T = {v: *("+source+" & !=\"bad\") | \"z\"}\n"+matrixConfig+"prefix: T.v", ""),
		detected("copy_closed_regex", matrixBase+"_c: close(system.units.producer)\n"+matrixConfig+"{if (_c.outputs.id & =~\"^x\") != _|_ {a: 1}}", ""),
		// Valid CUE that used to be refused because independence could not be established.
		edges("conditional_body_reference", matrixBase+"_e: true\n"+matrixConfig+"{if _e {name: "+source+"}}", "", matrixEdge),
		clean("for_let_interpolation", matrixBase+"_m: {a: \"b\"}\nsystem: units: producer: settings: tags: {for k, v in _m let z = \"p-\\(v)\" {(k): z}}", ""),
		clean("list_for_let_interpolation", matrixBase+"_xs: [\"a\"]\nsystem: units: producer: settings: l: [for y in _xs let z = \"p-\\(y)\" {z}]", ""),
		detected("presence_guard_on_reference", matrixImport+matrixBase+matrixConfig+"schema.#C & {name: "+source+"}", "#C: {name?: string, if name != _|_ {has: true}}"),
		detected("default_over_present_output", matrixBase+"_x: {a?: string, a: "+source+"}\nsystem: units: consumer: settings: name: *_x.a | \"fallback\"", ""),
		detected("default_over_narrowed_optional", matrixBase+"_x: {a?: string, a: "+source+" & \"x\"}\nsystem: units: consumer: settings: name: *_x.a | \"fallback\"", ""),
		detected("list_concat", "import \"list\"\n"+matrixBase+matrixConfig+"items: list.Concat([[{"+dynamic+"}]])", ""),
	}
	return append(cases, checks...)
}

func matrixSources(t *testing.T, snapshot string) []valuePath {
	t.Helper()
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		t.Fatal(err)
	}
	value, err := graph.build()
	if err != nil {
		t.Fatalf("fixture does not evaluate: %v", err)
	}
	var sources []valuePath
	for _, namespace := range []string{"units", "lookups"} {
		entries, err := value.LookupPath(cue.ParsePath("system." + namespace)).Fields()
		if err != nil {
			continue
		}
		for entries.Next() {
			outputs, err := entries.Value().LookupPath(cue.ParsePath("outputs")).Fields()
			if err != nil {
				continue
			}
			for outputs.Next() {
				sources = append(sources, frozenPath(fmt.Sprintf("system.%s.%s.outputs.%s", namespace, entries.Selector(), outputs.Selector())))
			}
		}
	}
	return sources
}

// Detection means a dependency rule fired, not that inspection gave up.
var errInspection = errors.New("inspection failure")

func matrixVerdict(t *testing.T, snapshot string) (string, []string, error) {
	t.Helper()
	sources := matrixSources(t, snapshot)
	var graph frozenGraph
	if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
		t.Fatal(err)
	}
	value, err := graph.buildAnalysis()
	if err != nil {
		t.Fatal(err)
	}
	referenceAnalysis, err := newReferenceGraph(value, &sources)
	if err != nil {
		return "", nil, fmt.Errorf("%w: %v", errInspection, err)
	}
	var references []frozenReference
	for _, root := range []string{"system.units", "system.lookups"} {
		found, err := referenceAnalysis.frozenReferences(cue.ParsePath(root))
		if err != nil {
			return "", nil, fmt.Errorf("%w: %v", errInspection, err)
		}
		references = append(references, found...)
	}
	var found []string
	unsupported := false
	for _, reference := range references {
		if reference.PrimitiveType == "" {
			continue
		}
		path, _ := reference.Path.cuePath()
		target, _ := reference.Target.cuePath()
		arrow := "<-"
		if reference.Computed {
			arrow = "<="
		}
		found = append(found, path.String()+arrow+target.String())
		unsupported = unsupported || !reference.Supported
	}
	sort.Strings(found)
	switch {
	case unsupported:
		return "detected", nil, nil
	case len(found) > 0:
		return "edges", found, nil
	}
	return "clean", nil, nil
}

func TestFrozenProvenanceMatrix(t *testing.T) {
	for _, test := range matrixCases() {
		t.Run(test.name, func(t *testing.T) {
			parts := strings.Split(test.app, "|||")
			files := map[string]string{"env.cue": "package app\n" + parts[0]}
			if len(parts) > 1 {
				files["more.cue"] = "package app\n" + parts[1]
			}
			if test.schema != "" {
				files["schema/types.cue"] = "package schema\n" + test.schema
			}
			_, snapshot := freezeTestModule(t, files)
			verdict, found, err := matrixVerdict(t, snapshot)
			if test.want == "refused" {
				if err == nil && verdict != "detected" && verdict != "incomplete" {
					t.Fatalf("verdict %s, want a refusal (edges %v)", verdict, found)
				}
				return
			}
			if err != nil {
				t.Fatalf("want %s: %v", test.want, err)
			}
			if verdict != test.want {
				t.Fatalf("verdict %s, want %s (edges %v)", verdict, test.want, found)
			}
			if test.want == "edges" {
				expected := append([]string{}, test.edges...)
				sort.Strings(expected)
				if strings.Join(found, ",") != strings.Join(expected, ",") {
					t.Fatalf("edges %v, want %v", found, expected)
				}
			}
		})
	}
}

// Provenance uses sharing off and everything exported uses a normal build, so both
// modes must agree on every accepted fixture, with and without real values.
func TestFrozenSharingModesAgree(t *testing.T) {
	compared := 0
	for _, test := range matrixCases() {
		if test.want != "clean" && test.want != "edges" {
			continue
		}
		t.Run(test.name, func(t *testing.T) {
			parts := strings.Split(test.app, "|||")
			files := map[string]string{"env.cue": "package app\n" + parts[0]}
			if len(parts) > 1 {
				files["more.cue"] = "package app\n" + parts[1]
			}
			if test.schema != "" {
				files["schema/types.cue"] = "package schema\n" + test.schema
			}
			_, snapshot := freezeTestModule(t, files)
			var graph frozenGraph
			if err := json.Unmarshal([]byte(snapshot), &graph); err != nil {
				t.Fatal(err)
			}
			sources := matrixSources(t, snapshot)
			for _, fill := range []string{"", `"plain-1"`, `"x"`, "true", "7"} {
				read := func(value cue.Value, build string) string {
					var fills []scalarFill
					if fill != "" {
						for _, source := range sources {
							path, err := source.cuePath()
							if err != nil {
								t.Fatal(err)
							}
							if value.LookupPath(path).IncompleteKind()&value.Context().CompileString(fill).IncompleteKind() == cue.BottomKind {
								return "unusable"
							}
							fills = append(fills, scalarFill{Path: source, Value: json.RawMessage(fill)})
						}
					}
					filled, _, err := fillScalars(value, fills)
					if err != nil {
						t.Fatalf("%s: %v", build, err)
					}
					leaves := map[string]string{}
					generatedRead(filled, cue.Path{}, leaves)
					encoded, err := json.Marshal(leaves)
					if err != nil {
						t.Fatal(err)
					}
					return string(encoded) + fmt.Sprint(filled.Validate() == nil)
				}
				shared, err := graph.build()
				if err != nil {
					t.Fatal(err)
				}
				unshared, err := graph.buildAnalysis()
				if err != nil {
					t.Fatal(err)
				}
				left, right := read(shared, "shared"), read(unshared, "unshared")
				if left != right {
					t.Fatalf("modes differ with fill %s:\n%s\n%s", fill, left, right)
				}
				compared++
			}
		})
	}
	if compared < 100 {
		t.Fatalf("only %d comparisons", compared)
	}
}

// A consumer of an un-unified imported alias must pass every operation, with and
// without a prepared capability, and they must return the same value.
func TestFrozenImportedAliasAcrossOperations(t *testing.T) {
	_, snapshot := freezeTestModule(t, map[string]string{
		"env.cue":          "package app\n" + matrixImport + "system: schema.#S\n",
		"schema/types.cue": "package schema\n" + stack("name: _i"),
	})
	sources := []valuePath{frozenPath(matrixSource)}
	roots := []valuePath{frozenPath("system.units")}
	projection := frozenPath("system.units.consumer.settings")
	consumer := frozenPath("system.units.consumer.settings.name")
	deferred := []deferredValue{{Path: consumer, Target: frozenPath(matrixSource)}}
	validation := validateFrozenResponse(snapshot, frozenJSON(t, frozenValidationOptions{ConcretePaths: []valuePath{projection}, Deferred: deferred, SourcePaths: &sources}))
	if strings.Contains(validation, `"error"`) {
		t.Fatalf("strict validation: %s", validation)
	}
	fill := []scalarFill{{Path: frozenPath(matrixSource), Value: json.RawMessage(`"plain-1"`)}}
	specialized := specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Fills: fill, Projection: projection, SourcePaths: &sources, ReferencePaths: roots}))
	if !strings.Contains(specialized, `"plain-1"`) {
		t.Fatalf("specialization: %s", specialized)
	}
	unknown := specializeFrozenResponse(snapshot, frozenJSON(t, specializationOptions{Projection: projection, Deferred: deferred, SourcePaths: &sources, ReferencePaths: roots}))
	if strings.Contains(unknown, `"error"`) {
		t.Fatalf("unknown specialization: %s", unknown)
	}
	capability, err := prepareFrozen(snapshot, frozenJSON(t, preparationOptions{SourcePaths: &sources, ReferencePaths: roots, Projections: []valuePath{projection}}))
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	prepared, err := specializePrepared(snapshot, capability, frozenJSON(t, preparedSpecializationOptions{Fills: fill, Projection: projection}))
	if err != nil {
		t.Fatalf("prepared: %v", err)
	}
	encoded, _ := json.Marshal(prepared.Value)
	if string(frozenPayload(t, specialized)) != `{"value":`+string(encoded)+`,"unresolved":[]}` {
		t.Fatalf("prepared %s differs from %s", encoded, frozenPayload(t, specialized))
	}
	// The reviewed escape stays rejected through preparation, not only inspection.
	_, escape := freezeTestModule(t, map[string]string{"env.cue": "package app\n" + matrixBase + matrixConfig + "close({key: \"k\", (*(" + matrixSource + " & \"version\") | \"mode\"): \"value\"})\n"})
	if _, err := prepareFrozen(escape, frozenJSON(t, preparationOptions{SourcePaths: &sources, ReferencePaths: roots, Projections: []valuePath{projection}})); err == nil {
		t.Fatal("preparation accepted an output-dependent dynamic label")
	}
}
