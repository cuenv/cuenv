package main

import (
	"fmt"
	"sort"

	"cuelang.org/go/cue"
)

// contextField is the top-level field that holds named contexts. Each entry
// has a `when` constraint over the facts of an invocation.
const contextField = "contexts"

// contextWhenField is the constraint of one named context.
const contextWhenField = "when"

// contextOverlapsField lists, per context, the other contexts whose
// constraint can hold at the same time.
const contextOverlapsField = "overlaps"

// maxContextMatcherDepth bounds the walk over a constraint, so a constraint
// that refers to itself is reported instead of followed forever.
const maxContextMatcherDepth = 64

// attachContextMatchers replaces every `contexts.<name>.when` of an exported
// instance with a matcher the caller can evaluate without CUE, and adds
// `contexts.<name>.overlaps`.
//
// A constraint is not data: buildValueClean exports `=~"^deploy"` and
// `"apply" | "destroy"` as null. The matcher keeps the constraint as a small
// tree of literals, kinds, regular expressions, disjunctions, conjunctions and
// fields. A constraint outside that set is an error, never a guess: a context
// that selects credentials must not match by accident.
//
// exported is left alone when it has no contexts, which also covers a
// projection that does not export them.
func attachContextMatchers(v cue.Value, exported map[string]interface{}) error {
	entries, ok := exported[contextField].(map[string]interface{})
	if !ok || len(entries) == 0 {
		return nil
	}
	names := make([]string, 0, len(entries))
	for name := range entries {
		names = append(names, name)
	}
	sort.Strings(names)

	constraints := make(map[string]cue.Value, len(names))
	for _, name := range names {
		entry, ok := entries[name].(map[string]interface{})
		if !ok {
			continue
		}
		when := v.LookupPath(cue.MakePath(cue.Str(contextField), cue.Str(name), cue.Str(contextWhenField)))
		if !when.Exists() {
			continue
		}
		if err := when.Validate(); err != nil {
			return fmt.Errorf("%s.%s.%s: %v", contextField, name, contextWhenField, err)
		}
		matcher, err := contextMatcher(when, 0)
		if err != nil {
			return fmt.Errorf("%s.%s.%s: %v", contextField, name, contextWhenField, err)
		}
		entry[contextWhenField] = matcher
		constraints[name] = when
	}
	for _, name := range names {
		entry, ok := entries[name].(map[string]interface{})
		if !ok {
			continue
		}
		constraint, ok := constraints[name]
		if !ok {
			continue
		}
		overlaps := []string{}
		for _, other := range names {
			otherConstraint, ok := constraints[other]
			if !ok || other == name {
				continue
			}
			if constraint.Unify(otherConstraint).Validate() == nil {
				overlaps = append(overlaps, other)
			}
		}
		entry[contextOverlapsField] = overlaps
	}
	return nil
}

// contextMatcher compiles one constraint into its matcher.
func contextMatcher(v cue.Value, depth int) (map[string]interface{}, error) {
	if depth > maxContextMatcherDepth {
		return nil, fmt.Errorf("the constraint is nested too deeply to match")
	}
	switch v.Kind() {
	case cue.NullKind:
		return map[string]interface{}{"equals": nil}, nil
	case cue.StringKind, cue.BoolKind, cue.IntKind, cue.FloatKind, cue.NumberKind:
		var literal interface{}
		if err := v.Decode(&literal); err != nil {
			return nil, fmt.Errorf("the literal %v could not be exported: %v", v, err)
		}
		return map[string]interface{}{"equals": literal}, nil
	case cue.StructKind:
		fields := make(map[string]interface{})
		iter, err := v.Fields()
		if err != nil {
			return nil, err
		}
		for iter.Next() {
			matcher, err := contextMatcher(iter.Value(), depth+1)
			if err != nil {
				return nil, fmt.Errorf("%s: %v", iter.Selector().Unquoted(), err)
			}
			fields[iter.Selector().Unquoted()] = matcher
		}
		return map[string]interface{}{"fields": fields}, nil
	case cue.ListKind:
		return nil, fmt.Errorf("lists cannot be matched")
	}

	op, operands := v.Expr()
	switch op {
	case cue.OrOp:
		return contextMatcherGroup("anyOf", operands, depth)
	case cue.AndOp:
		return contextMatcherGroup("allOf", operands, depth)
	case cue.RegexMatchOp, cue.NotRegexMatchOp, cue.NotEqualOp:
		if len(operands) != 1 {
			return nil, fmt.Errorf("the constraint %v cannot be matched", v)
		}
		return contextComparison(op, operands[0])
	case cue.SelectorOp, cue.IndexOp:
		target := cue.Dereference(v)
		if targetOp, _ := target.Expr(); targetOp == op {
			return nil, fmt.Errorf("the reference %v cannot be followed", v)
		}
		return contextMatcher(target, depth+1)
	case cue.NoOp:
		switch v.IncompleteKind() {
		case cue.StringKind:
			return map[string]interface{}{"kind": "string"}, nil
		case cue.BoolKind:
			return map[string]interface{}{"kind": "bool"}, nil
		case cue.IntKind, cue.FloatKind, cue.NumberKind:
			return map[string]interface{}{"kind": "number"}, nil
		case cue.TopKind:
			return map[string]interface{}{"kind": "any"}, nil
		}
	}
	return nil, fmt.Errorf("the constraint %v cannot be matched; use literals, =~, !~, !=, | and &", v)
}

func contextMatcherGroup(key string, operands []cue.Value, depth int) (map[string]interface{}, error) {
	matchers := make([]interface{}, 0, len(operands))
	for _, operand := range operands {
		matcher, err := contextMatcher(operand, depth+1)
		if err != nil {
			return nil, err
		}
		matchers = append(matchers, matcher)
	}
	return map[string]interface{}{key: matchers}, nil
}

func contextComparison(op cue.Op, operand cue.Value) (map[string]interface{}, error) {
	if op == cue.NotEqualOp {
		if !operand.IsConcrete() || operand.Kind() == cue.StructKind || operand.Kind() == cue.ListKind {
			return nil, fmt.Errorf("!= needs a literal, got %v", operand)
		}
		var literal interface{}
		if err := operand.Decode(&literal); err != nil {
			return nil, fmt.Errorf("the literal %v could not be exported: %v", operand, err)
		}
		return map[string]interface{}{"notEquals": literal}, nil
	}
	pattern, err := operand.String()
	if err != nil {
		return nil, fmt.Errorf("a regular expression must be a string literal, got %v", operand)
	}
	if op == cue.RegexMatchOp {
		return map[string]interface{}{"matches": pattern}, nil
	}
	return map[string]interface{}{"notMatches": pattern}, nil
}
