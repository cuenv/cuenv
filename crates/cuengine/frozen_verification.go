package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"maps"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/parser"
)

// frozenVerificationOptions names what real runtime values may change. Analysis
// says what depends on a source. This check says what real values do to the instance,
// which also catches a constraint they violate after other changes were applied.
type frozenVerificationOptions struct {
	Fills []scalarFill `json:"fills"`
	// Roots are compared with and without the fills.
	Roots []valuePath `json:"roots"`
	// Mutable are subtrees the fills may legitimately change.
	Mutable []valuePath `json:"mutable"`
	// References are leaves which must equal their target once it is filled.
	References []deferredValue `json:"references"`
}

//export cue_verify_frozen
func cue_verify_frozen(snapshot, options *C.char) *C.char {
	return C.CString(verifyFrozenResponse(C.GoString(snapshot), C.GoString(options)))
}

func verifyFrozenResponse(snapshot, optionsJSON string) (result string) {
	defer func() {
		if recover() != nil {
			result = frozenFailure(ErrorCodePanicRecover, "CUE verification failed")
		}
	}()
	var graph frozenGraph
	var options frozenVerificationOptions
	if json.Unmarshal([]byte(snapshot), &graph) != nil || json.Unmarshal([]byte(optionsJSON), &options) != nil {
		return frozenFailure(ErrorCodeInvalidInput, "Invalid CUE verification options")
	}
	base, err := graph.build()
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "Frozen CUE instance could not be evaluated")
	}
	if verifyFrozenInstance(base, options) != nil {
		return frozenFailure(ErrorCodeBuildValue, "Runtime values changed CUE outside the declared references")
	}
	return createSuccessResponse("true")
}

func verifyFrozenInstance(base cue.Value, options frozenVerificationOptions) error {
	filled, literals, err := fillScalars(base, options.Fills)
	if err != nil {
		return err
	}
	if filled.Validate() != nil {
		return errors.New("runtime values conflict with CUE constraints")
	}
	return compareFilled(filledComparison{base: base, filled: filled, literals: literals, roots: options.Roots, mutable: options.Mutable, references: options.References})
}

// Validation has rejected values that conflict before this comparison.
type filledComparison struct {
	base, filled cue.Value
	literals     map[string]cue.Value
	roots        []valuePath
	mutable      []valuePath
	references   []deferredValue
}

func compareFilled(comparison filledComparison) error {
	mutable := make([]cue.Path, 0, len(comparison.mutable)+len(comparison.references))
	for _, typed := range comparison.mutable {
		path, err := typed.cuePath()
		if err != nil {
			return err
		}
		mutable = append(mutable, path)
	}
	for _, reference := range comparison.references {
		path, err := reference.Path.cuePath()
		if err != nil {
			return err
		}
		mutable = append(mutable, path)
		target, err := reference.Target.cuePath()
		if err != nil {
			return err
		}
		literal, filled := comparison.literals[target.String()]
		if !filled {
			continue
		}
		leaf := comparison.filled.LookupPath(path)
		if !leaf.Exists() {
			return errors.New("reference leaf disappeared")
		}
		if leaf.Err() != nil || leaf.Validate() != nil {
			return errors.New("reference leaf conflicts with its value")
		}
		if !carries(leaf, literal) {
			return errors.New("reference leaf does not carry its value")
		}
	}
	for _, typed := range comparison.roots {
		path, err := typed.cuePath()
		if err != nil {
			return err
		}
		before, err := exportedLeaves(comparison.base.LookupPath(path), path, mutable)
		if err != nil {
			return err
		}
		after, err := exportedLeaves(comparison.filled.LookupPath(path), path, mutable)
		if err != nil {
			return err
		}
		if !maps.Equal(before, after) {
			return errors.New("values changed outside the declared references")
		}
	}
	return nil
}

// carries compares what leaves CUE: a scalar embedded next to hidden fields does not
// equal the bare scalar, but exports as one.
func carries(leaf, literal cue.Value) bool {
	left, leftErr := leaf.MarshalJSON()
	right, rightErr := literal.MarshalJSON()
	return leftErr == nil && rightErr == nil && bytes.Equal(left, right)
}

// fillScalars unifies every value at once: filling one path at a time costs a full
// re-evaluation each and grows with the size of the instance.
func fillScalars(base cue.Value, fills []scalarFill) (cue.Value, map[string]cue.Value, error) {
	literals := make(map[string]cue.Value, len(fills))
	tree := make(map[string]any)
	filled := base
	for _, fill := range fills {
		path, err := fill.Path.cuePath()
		if err != nil || len(path.Selectors()) == 0 || primitiveDeclaration(base.LookupPath(path)) == "" {
			return cue.Value{}, nil, errors.New("invalid runtime source")
		}
		if _, duplicate := literals[path.String()]; duplicate {
			return cue.Value{}, nil, errors.New("duplicate runtime source")
		}
		expression, err := scalarExpression(fill.Value)
		if err != nil {
			return cue.Value{}, nil, err
		}
		literal := base.Context().BuildExpr(expression)
		if literal.Validate(cue.Concrete(true), cue.Final()) != nil {
			return cue.Value{}, nil, errors.New("invalid runtime value")
		}
		literals[path.String()] = literal
		if labels := regularLabels(path); labels != nil {
			setNested(tree, labels, expression)
		} else {
			filled = filled.FillPath(path, literal)
		}
	}
	if len(tree) > 0 {
		filled = filled.FillPath(cue.Path{}, base.Context().BuildExpr(treeExpression(tree)))
	}
	return filled, literals, nil
}

func treeExpression(tree map[string]any) ast.Expr {
	fields := make([]any, 0, 2*len(tree))
	for label, value := range tree {
		fields = append(fields, ast.NewString(label))
		switch value := value.(type) {
		case map[string]any:
			fields = append(fields, treeExpression(value))
		case ast.Expr:
			fields = append(fields, value)
		}
	}
	return ast.NewStruct(fields...)
}

// scalarExpression keeps numbers exact and accepts any string. Compiling a string
// as CUE source would refuse characters such as a byte order mark.
func scalarExpression(raw json.RawMessage) (ast.Expr, error) {
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	var scalar any
	if decoder.Decode(&scalar) != nil {
		return nil, errors.New("invalid scalar value")
	}
	switch scalar := scalar.(type) {
	case string:
		return ast.NewString(scalar), nil
	case bool:
		return ast.NewBool(scalar), nil
	case json.Number:
		return parser.ParseExpr("", scalar.String())
	}
	return nil, errors.New("source value is not a primitive")
}

// exportedLeaves records every exported field and list element. Hidden values do
// not leave CUE; their conflicts are covered by validation.
func exportedLeaves(value cue.Value, root cue.Path, mutable []cue.Path) (map[string]string, error) {
	leaves := make(map[string]string)
	var visit func(cue.Value, cue.Path) error
	visit = func(current cue.Value, path cue.Path) error {
		for _, exempt := range mutable {
			if pathWithin(path, exempt) {
				return nil
			}
		}
		if !current.Exists() {
			return nil
		}
		child := func(selector cue.Selector) cue.Path {
			return cue.MakePath(append(append([]cue.Selector{}, path.Selectors()...), selector)...)
		}
		switch current.IncompleteKind() {
		case cue.StructKind:
			fields, err := current.Fields(cue.Definitions(false))
			if err != nil {
				leaves[path.String()] = "unreadable " + fmt.Sprint(current)
				return nil
			}
			leaves[path.String()] = "struct"
			for fields.Next() {
				if err := visit(fields.Value(), child(fields.Selector())); err != nil {
					return err
				}
			}
			return nil
		case cue.ListKind:
			items, err := current.List()
			if err != nil {
				leaves[path.String()] = "abstract " + fmt.Sprint(current)
				return nil
			}
			leaves[path.String()] = "list"
			for index := 0; items.Next(); index++ {
				if err := visit(items.Value(), child(cue.Index(index))); err != nil {
					return err
				}
			}
			return nil
		}
		encoded, err := current.MarshalJSON()
		if err != nil {
			// A non-concrete value compares by what it permits, not by a failure.
			leaves[path.String()] = "abstract " + fmt.Sprint(current)
			return nil
		}
		var compact bytes.Buffer
		if json.Compact(&compact, encoded) != nil {
			return errors.New("invalid leaf")
		}
		leaves[path.String()] = compact.String()
		return nil
	}
	return leaves, visit(value, root)
}
