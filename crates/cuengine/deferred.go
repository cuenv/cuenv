package main

import (
	"fmt"

	"cuelang.org/go/cue"
)

func regularLabels(path cue.Path) []string {
	labels := make([]string, 0, len(path.Selectors()))
	for _, selector := range path.Selectors() {
		if selector.LabelType() != cue.StringLabel || selector.ConstraintType() != 0 {
			return nil
		}
		labels = append(labels, selector.Unquoted())
	}
	return labels
}

// A declaration must be exactly one abstract primitive, without a default or
// constraints that would be lost when its runtime value is supplied.
func primitiveDeclaration(value cue.Value) string {
	var primitive string
	switch value.IncompleteKind() {
	case cue.StringKind:
		primitive = "string"
	case cue.NumberKind:
		primitive = "number"
	case cue.BoolKind:
		primitive = "bool"
	default:
		return ""
	}
	if _, hasDefault := value.Default(); hasDefault {
		return ""
	}
	if value.Subsume(value.Context().CompileString(primitive)) != nil {
		return ""
	}
	return primitive
}

func validateAbstractNamespace(value cue.Value, path cue.Path) error {
	if value.Kind() != cue.StructKind {
		return fmt.Errorf("%s: abstract namespace must be a struct", path)
	}
	fields, err := value.Fields(cue.Definitions(false))
	if err != nil {
		return err
	}
	for fields.Next() {
		if primitiveDeclaration(fields.Value()) == "" {
			return fmt.Errorf("%s: abstract declarations must be bare string, number or bool types without defaults or refinements", cue.MakePath(append(path.Selectors(), fields.Selector())...))
		}
	}
	return nil
}
