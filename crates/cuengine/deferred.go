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
	return validateAbstractFields(value, path)
}

// validateAbstractFields checks every regular field of a struct is an abstract
// declaration: a bare primitive, or a struct or list whose existing elements are.
func validateAbstractFields(value cue.Value, path cue.Path) error {
	fields, err := value.Fields(cue.Definitions(false))
	if err != nil {
		return err
	}
	for fields.Next() {
		child := cue.MakePath(append(path.Selectors(), fields.Selector())...)
		if err := validateAbstractDeclaration(fields.Value(), child); err != nil {
			return err
		}
	}
	return nil
}

// validateAbstractDeclaration accepts a bare primitive, a struct of
// declarations, or a list of declarations. A list whose length is open has no
// element that can be selected, so only its known elements are checked.
func validateAbstractDeclaration(value cue.Value, path cue.Path) error {
	if primitiveDeclaration(value) != "" {
		return nil
	}
	switch value.IncompleteKind() {
	case cue.StructKind:
		return validateAbstractFields(value, path)
	case cue.ListKind:
		if value.Len().Validate(cue.Concrete(true)) != nil {
			return nil
		}
		items, err := value.List()
		if err != nil {
			return err
		}
		for index := 0; items.Next(); index++ {
			child := cue.MakePath(append(path.Selectors(), cue.Index(index))...)
			if err := validateAbstractDeclaration(items.Value(), child); err != nil {
				return err
			}
		}
		return nil
	}
	return fmt.Errorf("%s: abstract declarations must be bare string, number or bool types without defaults or refinements, or lists and structs of them", path)
}
