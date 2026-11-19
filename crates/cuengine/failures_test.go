package main

import (
	"strings"
	"testing"
)

func TestNamedPackageEvaluationReportsInvalidModuleLanguage(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"cue.mod/module.cue": "module: \"example.com/bridge@v0\"\nlanguage: version: \"v999.0.0\"\n",
		"env.cue":            packageSource("name: \"example\""),
	})
	bridgeError := evaluation{moduleRoot: moduleRoot, packageName: packageNamed(testPackageName)}.failure(t)
	assertContains(t, bridgeError.Message, "module.cue", "v999.0.0", "too new")
}

func writeTwoErrorModule(t *testing.T) string {
	t.Helper()
	return writeCueModule(t, map[string]string{
		"broken/values.cue": packageSource("#Config: close({size: int, name: string})\n" +
			"config: #Config & {sise: 1, nmae: \"x\"}"),
	})
}

func TestInstanceFailures_ReportEveryErrorWithItsPosition(t *testing.T) {
	// A plain %v of a CUE error list prints only its first error and no
	// positions; both misspelled fields must be named, with file and line.
	moduleRoot := writeTwoErrorModule(t)
	strict := evaluation{
		moduleRoot:       moduleRoot,
		recursive:        true,
		packageName:      packageNamed(testPackageName),
		instanceFailures: InstanceFailuresFail,
	}
	bridgeError := strict.failure(t)
	assertContains(t, bridgeError.Message,
		"1 instance(s) could not be evaluated",
		"broken: config.",
		"config.sise: field not allowed",
		"config.nmae: field not allowed",
		"./broken/values.cue:4:",
	)
	if strings.Contains(bridgeError.Message, moduleRoot) {
		t.Errorf("positions should be relative to the module root: %s", bridgeError.Message)
	}
	if bridgeError.Hint != nil {
		t.Errorf("evaluation failures carry no hint, got %q", *bridgeError.Hint)
	}
}

func TestNoInstanceEvaluated_ListsTheFailuresInTheMessage(t *testing.T) {
	// With the default policy, a module where nothing evaluates fails with
	// every error in the message rather than a debugging hint.
	moduleRoot := writeTwoErrorModule(t)
	bridgeError := evaluation{moduleRoot: moduleRoot, recursive: true}.failure(t)
	assertErrorCode(t, bridgeError, ErrorCodeBuildValue)
	assertContains(t, bridgeError.Message,
		"No instances could be evaluated:",
		"config.sise: field not allowed",
		"config.nmae: field not allowed",
	)
	if bridgeError.Hint != nil {
		t.Errorf("evaluation failures carry no hint, got %q", *bridgeError.Hint)
	}
}
