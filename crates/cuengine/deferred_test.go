package main

import (
	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/load"
	"reflect"
	"testing"
)

func TestDirectReference(t *testing.T) {
	source := `package app
 let output = infrastructure.resources.producer.outputs.id
sourceAlias: infrastructure.resources.producer.outputs.id
 infrastructure: resources: {
 producer: outputs: id: string
 consumer: configuration: {
 direct: infrastructure.resources.producer.outputs.id
 alias: output
fieldAlias: sourceAlias
 conjunction: string & infrastructure.resources.producer.outputs.id
 typed: (string | {fromResource!: string, attribute!: string}) & infrastructure.resources.producer.outputs.id
 interpolation: "prefix-\(infrastructure.resources.producer.outputs.id)"
 arithmetic: infrastructure.resources.producer.outputs.number + 1
 constrained: =~"^prefix" & infrastructure.resources.producer.outputs.id
constrained_union: (number | =~"^prefix") & infrastructure.resources.producer.outputs.id
 }
 producer: outputs: number: number
 }
 `
	instance := cuecontext.New().CompileString(source)
	if instance.Err() != nil {
		t.Fatal(instance.Err())
	}
	for _, name := range []string{"direct", "alias", "fieldAlias", "conjunction", "typed", "interpolation", "arithmetic", "constrained", "constrained_union"} {
		t.Run(name, func(t *testing.T) {
			value := instance.LookupPath(cue.ParsePath("infrastructure.resources.consumer.configuration." + name))
			actual, err := directReference(value, instance)
			if err != nil {
				t.Fatal(err)
			}
			if name == "interpolation" || name == "arithmetic" || name == "constrained" || name == "constrained_union" {
				if actual != nil {
					t.Fatalf("unsupported expression extracted: %v", actual)
				}
				return
			}
			expected := []string{"infrastructure", "resources", "producer", "outputs", "id"}
			if !reflect.DeepEqual(actual, expected) {
				t.Fatalf("expected %v, got %v", expected, actual)
			}
		})
	}
}

func TestDeferredConcreteValidation(t *testing.T) {
	expected := []string{"resources", "producer", "outputs", "id"}
	options := &ReferenceEvaluation{AbstractPaths: []string{"resources.producer.outputs"}, Deferred: map[string][]string{"resources.consumer.configuration.id": expected}}
	paths, err := parseConcretePaths([]string{"resources"})
	if err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name, extra string
		fail        bool
	}{
		{"reference", "", false},
		{"ordinary incomplete", ", other: string", true},
		{"required field", ", missing!: string", true},
		{"open list", ", values: [...string]", true},
	} {
		t.Run(test.name, func(t *testing.T) {
			instance := cuecontext.New().CompileString(`resources: { producer: outputs: id: string, consumer: configuration: {id: resources.producer.outputs.id` + test.extra + `}}`)
			err := validateConcreteExcept(concreteValidation{instance: instance, paths: paths}, options)
			if (err != nil) != test.fail {
				t.Fatalf("failure=%t: %v", test.fail, err)
			}
		})
	}
	instance := cuecontext.New().CompileString(`resources: { producer: outputs: id: string, consumer: configuration: {id: string}}`)
	if err := validateConcreteExcept(concreteValidation{instance: instance, paths: paths}, options); err == nil {
		t.Fatal("stale exemption accepted")
	}
}

func TestAbstractOutputDeclarations(t *testing.T) {
	for _, test := range []struct {
		expression string
		valid      bool
	}{
		{"string", true}, {"number", true}, {"bool", true},
		{`string & =~"^prefix"`, false}, {"number & >0", false},
		{`"literal"`, false}, {`*"default" | string`, false},
		{"[...string]", false}, {"string | number", false},
	} {
		t.Run(test.expression, func(t *testing.T) {
			value := cuecontext.New().CompileString("outputs: id: " + test.expression).LookupPath(cue.ParsePath("outputs"))
			err := validateAbstractNamespace(value, cue.ParsePath("outputs"))
			if (err == nil) != test.valid {
				t.Fatalf("valid=%t: %v", test.valid, err)
			}
		})
	}
}

func TestUnsupportedExpressionsRemainDetectable(t *testing.T) {
	instance := cuecontext.New().CompileString(`package app
 resources: {
 producer: outputs: id: string
 consumer: configuration: {
   narrowed: producer.outputs.id & "literal"
   union: (number | =~"^prefix") & producer.outputs.id
 }
 }
 `)
	if instance.Err() != nil {
		t.Fatal(instance.Err())
	}
	paths, err := parseProjectionPaths("paths", []string{"resources.consumer.configuration"})
	if err != nil {
		t.Fatal(err)
	}
	references, err := collectDirectReferences(instance, paths)
	if err != nil {
		t.Fatal(err)
	}
	seen := map[string]bool{}
	for _, reference := range references {
		seen[reference.Path] = true
		if reference.Supported {
			t.Fatalf("unsupported expression accepted: %+v", reference)
		}
	}
	for _, field := range []string{"narrowed", "union"} {
		if !seen["resources.consumer.configuration."+field] {
			t.Fatalf("missing unsupported expression %s: %+v", field, references)
		}
	}
}

func TestReferenceEvaluationBridgeBoundary(t *testing.T) {
	moduleRoot := writePackage(t, `resources: {
  producer: outputs: id: string
  consumer: configuration: "_argument": resources.producer.outputs.id
 }
 ordinary: resources.producer.outputs.id
 `)
	selected := exactPackage(moduleRoot)
	selected.concretePaths = []string{"resources"}
	selected.referenceEvaluation = &ReferenceEvaluation{
		Paths:         []string{"resources"},
		AbstractPaths: []string{"resources.producer.outputs"},
		Deferred:      map[string][]string{`resources.consumer.configuration."_argument"`: {"resources", "producer", "outputs", "id"}},
	}
	// Use the engine's canonical destination path for the second evaluation.
	scanning := selected
	scanning.concretePaths = nil
	result := scanning.result(t)
	var canonical string
	for _, reference := range result.DirectReferences["."] {
		if len(reference.Labels) == 4 && reference.Labels[3] == "_argument" {
			canonical = reference.Path
			if !reference.Supported || reference.PrimitiveType != "string" {
				t.Fatalf("bad metadata: %+v", reference)
			}
		}
	}
	if canonical == "" {
		t.Fatalf("missing destination labels: %+v", result.DirectReferences)
	}
	selected.referenceEvaluation.Deferred = map[string][]string{canonical: {"resources", "producer", "outputs", "id"}}
	selected.result(t)
	strict := selected
	strict.referenceEvaluation = nil
	strict.failure(t)
	selected.concretePaths = append(selected.concretePaths, "ordinary")
	selected.failure(t)
}

func TestPrimitiveDeclarationWithPatternConstraint(t *testing.T) {
	instance := cuecontext.New().CompileString(`#Root: close({outputs: {[string]: string|number|bool}})
value: #Root & {outputs: id: string}`)
	if instance.Err() != nil {
		t.Fatal(instance.Err())
	}
	value := instance.LookupPath(cue.ParsePath("value.outputs.id"))
	if primitiveDeclaration(value) != "string" {
		t.Fatal("primitive declaration lost through schema pattern")
	}
}

func TestDefaultedOutputReferenceRemainsDetectable(t *testing.T) {
	for _, expression := range []string{
		`*"fallback" | infrastructure.resources.primary.outputs.id`,
		`infrastructure.resources.primary.outputs.id | *"fallback"`,
		`*"fallback" | primary.outputs.id`,
		`*"fallback" | alias`,
		`*"fallback" | fieldAlias`,
	} {
		t.Run(expression, func(t *testing.T) {
			instance := cuecontext.New().CompileString(`package app
 let alias = infrastructure.resources.primary.outputs.id
 fieldAlias: alias
 infrastructure: resources: {
 primary: outputs: id: string
 consumer: configuration: name: ` + expression + `
 }
 `)
			if instance.Err() != nil {
				t.Fatal(instance.Err())
			}
			paths, err := parseProjectionPaths("paths", []string{"infrastructure.resources.consumer.configuration"})
			if err != nil {
				t.Fatal(err)
			}
			references, err := collectDirectReferences(instance, paths)
			if err != nil {
				t.Fatal(err)
			}
			if len(references) != 1 || references[0].Supported {
				t.Fatalf("defaulted output reference was lost or accepted: %+v", references)
			}
			expected := []string{"infrastructure", "resources", "primary", "outputs", "id"}
			if !reflect.DeepEqual(references[0].Target, expected) {
				t.Fatalf("noncanonical reference: %+v", references[0])
			}
		})
	}
}

func TestOrdinaryDefaultsRemainOrdinaryValues(t *testing.T) {
	instance := cuecontext.New().CompileString(`configuration: {name: *"fallback" | string, count: *1 | int}`)
	paths, err := parseProjectionPaths("paths", []string{"configuration"})
	if err != nil {
		t.Fatal(err)
	}
	references, err := collectDirectReferences(instance, paths)
	if err != nil {
		t.Fatal(err)
	}
	if len(references) != 0 {
		t.Fatalf("ordinary default acquired references: %+v", references)
	}
	value, err := instance.LookupPath(cue.ParsePath("configuration.name")).String()
	if err != nil || value != "fallback" {
		t.Fatalf("ordinary default changed: %q %v", value, err)
	}
}

func TestDefaultedOutputReferenceThroughShadowedBuiltin(t *testing.T) {
	for _, test := range []struct{ name, alias, primitive, expression string }{
		{"field", `string: infrastructure.resources.primary.outputs.id`, "number", `*42 | string`},
		{"let", `let number = infrastructure.resources.primary.outputs.id`, "string", `*"fallback" | number`},
		{"defaulted let", `let string = *42 | infrastructure.resources.primary.outputs.id`, "number", `string`},
	} {
		t.Run(test.name, func(t *testing.T) {
			instance := cuecontext.New().CompileString(`package app
 ` + test.alias + `
 infrastructure: resources: {
 primary: outputs: id: ` + test.primitive + `
 consumer: configuration: name: ` + test.expression + `
 }
 `)
			if instance.Err() != nil {
				t.Fatal(instance.Err())
			}
			paths, err := parseProjectionPaths("paths", []string{"infrastructure.resources.consumer.configuration"})
			if err != nil {
				t.Fatal(err)
			}
			references, err := collectDirectReferences(instance, paths)
			if err != nil {
				t.Fatal(err)
			}
			if len(references) != 1 || references[0].Supported {
				t.Fatalf("shadowed builtin hid output reference: %+v", references)
			}
		})
	}
}

func TestOrdinaryDefaultedStructsDoNotTreatLabelsAsReferences(t *testing.T) {
	for _, expression := range []string{
		`*{} | {field: string}`,
		`*{} | {field: int, another: bool}`,
		`*{} | {string: "literal", number: 1}`,
		`*{} | {"not_a_reference": string}`,
		`*{} | string`,
	} {
		t.Run(expression, func(t *testing.T) {
			instance := cuecontext.New().CompileString("configuration: name: " + expression)
			paths, err := parseProjectionPaths("paths", []string{"configuration"})
			if err != nil {
				t.Fatal(err)
			}
			references, err := collectDirectReferences(instance, paths)
			if err != nil {
				t.Fatal(err)
			}
			if len(references) != 0 {
				t.Fatalf("struct labels acquired references: %+v", references)
			}
		})
	}
}

func TestDefaultedContainerCannotHideOutputReferences(t *testing.T) {
	for _, expression := range []string{
		`*{} | {field: infrastructure.resources.primary.outputs.id}`,
		`*[] | [infrastructure.resources.primary.outputs.id]`,
	} {
		t.Run(expression, func(t *testing.T) {
			instance := cuecontext.New().CompileString(`infrastructure: resources: {
 primary: outputs: id: string
 consumer: configuration: name: ` + expression + `
}`)
			paths, err := parseProjectionPaths("paths", []string{"infrastructure.resources.consumer.configuration"})
			if err != nil {
				t.Fatal(err)
			}
			references, err := collectDirectReferences(instance, paths)
			if err != nil {
				t.Fatal(err)
			}
			if len(references) != 1 || references[0].Supported {
				t.Fatalf("defaulted container hid output reference: %+v", references)
			}
		})
	}
}

func TestDefaultedUnsupportedExpressionsFailClosed(t *testing.T) {
	for _, expression := range []string{
		`*"fallback" | infrastructure.resources.primary.outputs.id[0:2]`,
		`*[] | [for value in [infrastructure.resources.primary.outputs.id] {value}]`,
		`*[] | [for resource in [infrastructure.resources.primary] {resource.outputs.id}]`,
	} {
		t.Run(expression, func(t *testing.T) {
			instance := cuecontext.New().CompileString(`infrastructure: resources: {
 primary: outputs: id: string
 consumer: configuration: name: ` + expression + `
}`)
			paths, err := parseProjectionPaths("paths", []string{"infrastructure.resources.consumer.configuration"})
			if err != nil {
				t.Fatal(err)
			}
			references, err := collectDirectReferences(instance, paths)
			if err != nil {
				t.Logf("rejected unsupported expression: %v", err)
				return
			}
			if len(references) == 0 {
				t.Fatal("unsupported expression hid its output reference")
			}
			for _, reference := range references {
				if reference.Supported {
					t.Fatalf("unsupported expression accepted: %+v", reference)
				}
			}
		})
	}
}

func TestOrdinaryDefaultedComprehensionsRemainValid(t *testing.T) {
	for _, expression := range []string{
		`*[] | [for value in [1,2] {value}]`,
		`*[] | [for value in [{name: "literal"}] {value.name}]`,
		`*[] | [for value in [1,2] if value > 1 {value}]`,
		`*[] | [for value in [1,2] let doubled = value * 2 {doubled}]`,
		`*[] | [for item in [1] let alias = item {value: alias}]`,
		`*[] | [for value in [string] {value}]`,
	} {
		t.Run(expression, func(t *testing.T) {
			instance := cuecontext.New().CompileString("configuration: name: " + expression)
			paths, err := parseProjectionPaths("paths", []string{"configuration"})
			if err != nil {
				t.Fatal(err)
			}
			references, err := collectDirectReferences(instance, paths)
			if err != nil {
				t.Fatal(err)
			}
			if len(references) != 0 {
				t.Fatalf("ordinary comprehension acquired references: %+v", references)
			}
		})
	}
}

func TestDefaultedIndicesAndDynamicLabelsCannotHideOutputs(t *testing.T) {
	for _, expression := range []string{
		`*0 | [1,2][infrastructure.resources.primary.outputs.index]`,
		`*[] | [for values in [[1,2]] {values[infrastructure.resources.primary.outputs.index]}]`,
		`*{} | {"\(infrastructure.resources.primary.outputs.id)": "literal"}`,
	} {
		t.Run(expression, func(t *testing.T) {
			instance := cuecontext.New().CompileString(`infrastructure: resources: {
 primary: outputs: {id: string, index: number}
 consumer: configuration: name: ` + expression + `
}`)
			paths, err := parseProjectionPaths("paths", []string{"infrastructure.resources.consumer.configuration"})
			if err != nil {
				t.Fatal(err)
			}
			references, err := collectDirectReferences(instance, paths)
			if err != nil {
				return
			}
			if len(references) == 0 {
				t.Fatal("index or dynamic label hid its output reference")
			}
			for _, reference := range references {
				if reference.Supported {
					t.Fatalf("unsupported expression accepted: %+v", reference)
				}
			}
		})
	}
}

func TestImportedClosedResourceReferenceScanning(t *testing.T) {
	for _, test := range []struct {
		name, argument string
		references     int
		supported      bool
	}{
		{"no references", `"literal"`, 0, false},
		{"direct selector", `infrastructure.resources.primary.outputs.id`, 1, true},
		{"defaulted selector", `*"fallback" | infrastructure.resources.primary.outputs.id`, 1, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			moduleRoot := writeCueModule(t, map[string]string{
				"schema/reference.cue": `package schema
#Reference: close({fromResource!: string, attribute!: string})`,
				"provider/resource.cue": `package provider
import "example.com/bridge/schema"
#Resource: {type: "fake_plain", configuration: #Configuration, outputs: #Outputs, ...}
#Configuration: {name!: string | schema.#Reference, length!: number | schema.#Reference, "number"?: bool | schema.#Reference}
#Outputs: {id: string}`,
				"values.cue": packageSource(`import "example.com/bridge/provider"
infrastructure: resources: {
 primary: provider.#Resource & {configuration: {name: "primary", length: 1}}
 consumer: provider.#Resource & {configuration: {name: ` + test.argument + `, length: 1}}
}`),
			})
			loaded := load.Instances([]string{"."}, &load.Config{Dir: moduleRoot, Package: testPackageName})
			instance := cuecontext.New().BuildInstance(loaded[0])
			syntax := instance.LookupPath(cue.ParsePath("infrastructure.resources")).Syntax(cue.Raw())
			_, ok := syntax.(*ast.File)
			if !ok {
				t.Fatalf("regression fixture does not exercise a file syntax node: %T", syntax)
			}

			evaluation := exactPackage(moduleRoot)
			evaluation.referenceEvaluation = &ReferenceEvaluation{Paths: []string{"infrastructure.resources"}}
			result := evaluation.result(t)
			var outputs []DirectReference
			for _, reference := range result.DirectReferences["."] {
				if reflect.DeepEqual(reference.Target, []string{"infrastructure", "resources", "primary", "outputs", "id"}) {
					outputs = append(outputs, reference)
				}
			}
			if len(outputs) != test.references {
				t.Fatalf("expected %d selector records, got %+v", test.references, outputs)
			}
			for _, reference := range outputs {
				if reference.Supported != test.supported {
					t.Fatalf("supported=%t, got %+v", test.supported, reference)
				}
			}
		})
	}
}
