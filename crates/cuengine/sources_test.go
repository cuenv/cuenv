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
	assertContains(t, response, `"ok":{"files":["env.cue","infra/dns/env.cue","infra/dns/zones/zones.cue","shared/shared.cue"],"embeds":[]}`)

	missing := instanceSourcesResponse(moduleRoot, filepath.Join(moduleRoot, "infra/dns"), "absent")
	assertContains(t, missing, ErrorCodeLoadInstance)

	empty := instanceSourcesResponse(moduleRoot, "", "cuenv")
	assertContains(t, empty, ErrorCodeInvalidInput)
}

func TestInstanceSourcesListWhatEmbedAttributesRead(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"cue.mod/module.cue": "module: \"example.com/embeds@v0\"\nlanguage: version: \"v0.14.1\"\n",
		"env.cue":            "@extern(embed)\n\npackage cuenv\n\nroot: _ @embed(file=\"nix/source-files.json\")\n",
		"app/env.cue": "@extern(embed)\n\npackage cuenv\n\nimport \"example.com/embeds/shared\"\n\n" +
			"name: shared.name\nconfig: _ @embed(glob=config/*.yaml, allowEmptyGlob)\nnested: deep: _ @embed(file=data.json, type=json)\n" +
			"other: _ @tag(x) @embed(glob=\"a,b/*.txt\")\n",
		"shared/shared.cue": "@extern(embed)\n\npackage shared\n\nname: \"example\"\nlogo: _ @embed(file=\"logo.svg\", type=text)\n",
	})

	response := instanceSourcesResponse(moduleRoot, filepath.Join(moduleRoot, "app"), "cuenv")

	assertContains(t, response, `"embeds":["app/a,b/*.txt","app/config/*.yaml","app/data.json","nix/source-files.json","shared/logo.svg"]`)
}

func TestInstanceSourcesLeaveOutAnEmbedThatLeavesTheModule(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"cue.mod/module.cue": "module: \"example.com/embeds@v0\"\nlanguage: version: \"v0.14.1\"\n",
		"env.cue":            "@extern(embed)\n\npackage cuenv\n\nescapes: _ @embed(file=\"../outside.json\")\n",
	})

	response := instanceSourcesResponse(moduleRoot, moduleRoot, "cuenv")

	assertContains(t, response, `"embeds":[]`)
}
