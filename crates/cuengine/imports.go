package main

import (
	"strconv"

	"cuelang.org/go/cue"
	"cuelang.org/go/cue/ast"
	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/errors"
)

// validateImportedSelectors rejects a selection of a field an imported package
// does not declare, such as `schema.#Missing`.
//
// CUE treats such a selection as an incomplete value rather than an error, so
// validation accepts it and a projection that does not export the field never
// sees it: a misspelled or removed schema definition would otherwise silently
// leave the field that uses it unset. Only the first selector is checked,
// because that is the only one naming a package member; builtin packages are
// not in inst.Imports and CUE already rejects their unknown members.
func validateImportedSelectors(ctx *cue.Context, inst *build.Instance) error {
	imported := make(map[string]*build.Instance, len(inst.Imports))
	for _, dependency := range inst.Imports {
		imported[dependency.ImportPath] = dependency
	}
	packages := make(map[string]cue.Value, len(imported))

	var failures errors.Error
	for _, file := range inst.Files {
		ast.Walk(file, func(node ast.Node) bool {
			selector, isSelector := node.(*ast.SelectorExpr)
			if !isSelector {
				return true
			}
			base, isIdent := selector.X.(*ast.Ident)
			if !isIdent {
				return true
			}
			spec, isImport := base.Node.(*ast.ImportSpec)
			if !isImport {
				return true
			}
			importPath, err := strconv.Unquote(spec.Path.Value)
			if err != nil {
				return true
			}
			dependency, isBuilt := imported[importPath]
			if !isBuilt {
				return true
			}
			name, _, err := ast.LabelName(selector.Sel)
			if err != nil {
				return true
			}
			pkg, isCached := packages[importPath]
			if !isCached {
				pkg = ctx.BuildInstance(dependency)
				packages[importPath] = pkg
			}
			if !packageDeclares(pkg, name) {
				failures = errors.Append(failures, errors.Newf(
					selector.Sel.Pos(),
					"%s: undefined in package %q",
					name,
					importPath,
				))
			}
			return true
		}, nil)
	}
	return failures
}

func packageDeclares(pkg cue.Value, name string) bool {
	if len(name) > 0 && name[0] == '#' {
		return pkg.LookupPath(cue.MakePath(cue.Def(name))).Exists()
	}
	return pkg.LookupPath(cue.MakePath(cue.Str(name))).Exists() ||
		pkg.LookupPath(cue.MakePath(cue.Str(name).Optional())).Exists()
}
