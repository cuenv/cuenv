package main

import (
	"path/filepath"
	"reflect"
	"testing"
)

// writeNamedRootModule writes a module whose root directory is called name,
// with the test package at the root and in a child directory, and one
// directory of each kind CUE's "./..." walk leaves out. Directories without
// CUE files (.git, _empty) are never reported.
func writeNamedRootModule(t *testing.T, name string) string {
	t.Helper()
	return writeCueModuleAt(t, filepath.Join(t.TempDir(), name), map[string]string{
		"values.cue":                packageSource(`kind: "root"`),
		"child/values.cue":          packageSource(`child: true`),
		"_underscore/values.cue":    packageSource(`name: "underscore"`),
		".dot/deeper/values.cue":    packageSource(`name: "dot"`),
		"testdata/values.cue":       packageSource(`name: "testdata"`),
		"nested/cue.mod/module.cue": "module: \"example.com/nested@v0\"\nlanguage: version: \"v0.14.1\"\n",
		"nested/values.cue":         packageSource(`name: "nested"`),
		".git/HEAD":                 "ref: refs/heads/main\n",
		"_empty/readme.txt":         "no CUE files here\n",
	})
}

func TestRecursiveEvaluation_LoadsTheRootWhateverItsName(t *testing.T) {
	// CUE's own "./..." walk also applies its skip rules to the walk root,
	// so a module rooted at ".hidden-mod" or "_mod" loaded nothing.
	for _, name := range []string{".hidden-mod", "_mod", "testdata", "plain-mod"} {
		t.Run(name, func(t *testing.T) {
			moduleRoot := writeNamedRootModule(t, name)

			strict := allPackages(moduleRoot)
			strict.instanceFailures = InstanceFailuresFail
			assertInstanceKeys(t, strict.result(t), ".:app", "child:app")

			named := evaluation{moduleRoot: moduleRoot, recursive: true, packageName: packageNamed(testPackageName)}
			assertInstanceKeys(t, named.result(t), ".", "child")

			exact := exactPackage(moduleRoot)
			assertInstanceKeys(t, exact.result(t), ".")
		})
	}
}

func TestRecursiveEvaluation_LeavesOutTheSameDirectoriesAsCue(t *testing.T) {
	// Below the root, the walk keeps CUE's rules: dot, underscore, testdata
	// and nested-module directories are not loaded.
	moduleRoot := writeNamedRootModule(t, "module")
	assertInstanceKeys(t, allPackages(moduleRoot).result(t), ".:app", "child:app")
}

func TestSkippedDirectories_ReportListsLeftOutDirectoriesHoldingCueFiles(t *testing.T) {
	moduleRoot := writeNamedRootModule(t, "module")
	report := allPackages(moduleRoot)
	report.skippedDirectories = SkippedDirectoriesReport
	result := report.result(t)
	assertInstanceKeys(t, result, ".:app", "child:app")
	expected := []SkippedDirectory{
		{Path: ".dot", Reason: SkippedReasonDot},
		{Path: "_underscore", Reason: SkippedReasonUnderscore},
		{Path: "nested", Reason: SkippedReasonNestedModule},
		{Path: "testdata", Reason: SkippedReasonTestdata},
	}
	if !reflect.DeepEqual(result.SkippedDirectories, expected) {
		t.Errorf("expected skipped directories %+v, got %+v", expected, result.SkippedDirectories)
	}

	for _, mode := range []string{"", SkippedDirectoriesIgnore} {
		ignore := allPackages(moduleRoot)
		ignore.skippedDirectories = mode
		if skipped := ignore.result(t).SkippedDirectories; len(skipped) != 0 {
			t.Errorf("mode %q should not report skipped directories, got %+v", mode, skipped)
		}
	}
}

func TestSkippedDirectories_ReportedWhenNothingElseMatches(t *testing.T) {
	// A caller looking for a package that only exists in a left-out
	// directory gets an empty result and learns why.
	moduleRoot := writeCueModule(t, map[string]string{
		"_work/values.cue": packageSource(`name: "work"`),
		"other/values.cue": "package other\n\nname: \"other\"\n",
	})
	query := evaluation{
		moduleRoot:         moduleRoot,
		recursive:          true,
		packageName:        packageNamed(testPackageName),
		skippedDirectories: SkippedDirectoriesReport,
	}
	result := query.result(t)
	assertInstanceKeys(t, result)
	expected := []SkippedDirectory{{Path: "_work", Reason: SkippedReasonUnderscore}}
	if !reflect.DeepEqual(result.SkippedDirectories, expected) {
		t.Errorf("expected skipped directories %+v, got %+v", expected, result.SkippedDirectories)
	}
}

func TestSkippedDirectories_UnknownValueIsInvalidInput(t *testing.T) {
	moduleRoot := writeNamedRootModule(t, "module")
	unknown := allPackages(moduleRoot)
	unknown.skippedDirectories = "include"
	bridgeError := unknown.failure(t)
	assertErrorCode(t, bridgeError, ErrorCodeInvalidInput)
	assertContains(t, bridgeError.Message, `Unknown skippedDirectories value "include"`)
}

func TestPackageScopeAll_OmitsAnonymousInstancesWithoutFiles(t *testing.T) {
	// Loading every package also yields a file-less "_" instance next to
	// each package directory; only anonymous files someone wrote count.
	moduleRoot := writeMultiplePackageModule(t, map[string]string{
		"docs/readme.txt": "no CUE files here\n",
	})
	result := allPackages(moduleRoot).result(t)
	assertInstanceKeys(t, result,
		".:app", "child:app", "mixed:alpha", "mixed:beta", "single:single", "unnamed:_")
}
