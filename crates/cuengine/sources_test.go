package main

import (
	"path/filepath"
	"testing"
)

func TestInstanceSources(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"cue.mod/module.cue":             "module: \"example.com/sources@v0\"\nlanguage: version: \"v0.14.1\"\n",
		"env.cue":                        "package cuenv\n\nroot: true\n",
		"infra/dns/env.cue":              "package cuenv\n\nimport \"example.com/sources/infra/dns/zones\"\n\nzone: zones.all\n",
		"infra/dns/other.cue":            "package other\n\nignored: true\n",
		"infra/dns/zones/zones.cue":      "package zones\n\nimport \"example.com/sources/shared\"\n\nall: shared.name\n",
		"shared/shared.cue":              "package shared\n\nname: \"example\"\n",
		"unrelated/env.cue":              "package cuenv\n\nunrelated: true\n",
		"infra/dns/zones/zones_test.cue": "package zones_test\n",
	})

	response := instanceSourcesResponse(moduleRoot, filepath.Join(moduleRoot, "infra/dns"), "cuenv")
	assertContains(t, response, `"ok":{"files":["env.cue","infra/dns/env.cue","infra/dns/zones/zones.cue","shared/shared.cue"]}`)

	missing := instanceSourcesResponse(moduleRoot, filepath.Join(moduleRoot, "infra/dns"), "absent")
	assertContains(t, missing, ErrorCodeLoadInstance)

	empty := instanceSourcesResponse(moduleRoot, "", "cuenv")
	assertContains(t, empty, ErrorCodeInvalidInput)
}
