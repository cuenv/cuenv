package main

import (
	"encoding/json"
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
	assertContains(t, response, `"ok":{"files":["env.cue","infra/dns/env.cue","infra/dns/zones/zones.cue","shared/shared.cue"],"embeds":[],"module":"example.com/sources@v0","imports":[]}`)

	missing := instanceSourcesResponse(moduleRoot, filepath.Join(moduleRoot, "infra/dns"), "absent")
	assertContains(t, missing, ErrorCodeLoadInstance)

	empty := instanceSourcesResponse(moduleRoot, "", "cuenv")
	assertContains(t, empty, ErrorCodeInvalidInput)
}

// importsOf returns the records of the packages the instance in directory
// imports, asking for fields.
func importsOf(t *testing.T, moduleRoot string, directory string, options string) instanceSources {
	t.Helper()
	response := instanceSourcesResponseWith(sourcesRequest{
		moduleRoot:  moduleRoot,
		directory:   filepath.Join(moduleRoot, directory),
		packageName: "cuenv",
		options:     options,
	})
	var decoded struct {
		Ok    *instanceSources `json:"ok"`
		Error *BridgeError     `json:"error"`
	}
	if err := json.Unmarshal([]byte(response), &decoded); err != nil {
		t.Fatalf("decode %s: %v", response, err)
	}
	if decoded.Ok == nil {
		t.Fatalf("instance sources failed: %s", response)
	}
	return *decoded.Ok
}

func importNamed(t *testing.T, sources instanceSources, importPath string) importedInstance {
	t.Helper()
	for _, imported := range sources.Imports {
		if imported.ImportPath == importPath {
			return imported
		}
	}
	t.Fatalf("no import %s in %+v", importPath, sources.Imports)
	return importedInstance{}
}

func fieldText(fields map[string]json.RawMessage, name string) string {
	return string(fields[name])
}

func TestInstanceSourcesDescribeEveryImportedPackage(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"cue.mod/module.cue": "module: \"example.com/refs@v0\"\nlanguage: version: \"v0.14.1\"\n",
		"env.cue":            "package cuenv\n\nshared: true\n",
		// The root of `content` is in error; its name can still be read.
		"content/env.cue":        "package cuenv\n\nname: \"content\"\nbroken: 1\nbroken: 2\n",
		"content/drafts/env.cue": "package cuenv\n\ndrafts: true\n",
		"content/drafts/x.cue":   "package other\n\nname: \"not cuenv\"\n",
		"shared/shared.cue":      "package shared\n\nimport content \"example.com/refs/content:cuenv\"\n\nfiles: content.name\n",
		"other/env.cue":          "package other\n\nname: \"other\"\n",
		"defaulted/env.cue":      "package cuenv\n\nname: string | *\"defaulted\"\n",
		"incomplete/env.cue":     "package cuenv\n\nname: string\n",
		"app/env.cue": "package cuenv\n\nimport (\n" +
			"\tcontent \"example.com/refs/content:cuenv\"\n" +
			"\tdrafts \"example.com/refs/content/drafts:cuenv\"\n" +
			"\t\"example.com/refs/shared\"\n" +
			"\tother \"example.com/refs/other:other\"\n" +
			"\tdefaulted \"example.com/refs/defaulted:cuenv\"\n" +
			"\tincomplete \"example.com/refs/incomplete:cuenv\"\n" +
			")\n\nname: \"app\"\nuses: [content.broken, drafts.drafts, shared.files, other.name, defaulted.name, incomplete.name]\n",
	})

	sources := importsOf(t, moduleRoot, "app", `{"fields":["name"]}`)

	if sources.Module != "example.com/refs@v0" {
		t.Fatalf("module: %q", sources.Module)
	}
	content := importNamed(t, sources, "example.com/refs/content:cuenv")
	if content.Directory != "content" || content.Package != "cuenv" || content.Module != "example.com/refs@v0" || content.ImportedBy != "" {
		t.Fatalf("content: %+v", content)
	}
	if got := fieldText(content.Fields, "name"); got != `"content"` {
		t.Fatalf("a root in error still yields its name: %q", got)
	}
	if len(content.Ancestors) != 1 || content.Ancestors[0].Directory != "." || len(content.Ancestors[0].Fields) != 0 {
		t.Fatalf("content ancestors: %+v", content.Ancestors)
	}

	drafts := importNamed(t, sources, "example.com/refs/content/drafts:cuenv")
	if got := fieldText(drafts.Fields, "name"); got != `"content"` {
		t.Fatalf("a child directory inherits the name: %q", got)
	}
	if len(drafts.Ancestors) != 2 || drafts.Ancestors[0].Directory != "content" || fieldText(drafts.Ancestors[0].Fields, "name") != `"content"` || drafts.Ancestors[1].Directory != "." {
		t.Fatalf("drafts ancestors, nearest first: %+v", drafts.Ancestors)
	}

	other := importNamed(t, sources, "example.com/refs/other:other")
	if other.Package != "other" || fieldText(other.Fields, "name") != `"other"` || len(other.Ancestors) != 0 {
		t.Fatalf("other: %+v", other)
	}
	shared := importNamed(t, sources, "example.com/refs/shared")
	if len(shared.Fields) != 0 {
		t.Fatalf("a package without the field has none: %+v", shared)
	}
	if got := fieldText(importNamed(t, sources, "example.com/refs/defaulted:cuenv").Fields, "name"); got != `"defaulted"` {
		t.Fatalf("defaults are resolved: %q", got)
	}
	if fields := importNamed(t, sources, "example.com/refs/incomplete:cuenv").Fields; len(fields) != 0 {
		t.Fatalf("an incomplete field is left out: %+v", fields)
	}
}

func TestInstanceSourcesListTransitiveImportsOnce(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"cue.mod/module.cue": "module: \"example.com/refs@v0\"\nlanguage: version: \"v0.14.1\"\n",
		"content/env.cue":    "package cuenv\n\nname: \"content\"\n",
		"shared/shared.cue":  "package shared\n\nimport content \"example.com/refs/content:cuenv\"\n\nfiles: content.name\n",
		"app/env.cue":        "package cuenv\n\nimport \"example.com/refs/shared\"\n\nname: \"app\"\nuses: shared.files\n",
	})

	sources := importsOf(t, moduleRoot, "app", `{"fields":["name"]}`)

	if len(sources.Imports) != 2 {
		t.Fatalf("imports: %+v", sources.Imports)
	}
	content := importNamed(t, sources, "example.com/refs/content:cuenv")
	if content.ImportedBy != "example.com/refs/shared" || fieldText(content.Fields, "name") != `"content"` {
		t.Fatalf("a transitive import names its importer: %+v", content)
	}
}

func TestInstanceSourcesListNoImportsWithoutFields(t *testing.T) {
	moduleRoot := writeCueModule(t, map[string]string{
		"cue.mod/module.cue": "module: \"example.com/refs@v0\"\nlanguage: version: \"v0.14.1\"\n",
		"content/env.cue":    "package cuenv\n\nname: \"content\"\n",
		"app/env.cue":        "package cuenv\n\nimport content \"example.com/refs/content:cuenv\"\n\nname: content.name\n",
	})

	if sources := importsOf(t, moduleRoot, "app", ""); len(sources.Imports) != 0 {
		t.Fatalf("no fields asked for, no packages built: %+v", sources.Imports)
	}
	invalid := instanceSourcesResponseWith(sourcesRequest{
		moduleRoot:  moduleRoot,
		directory:   filepath.Join(moduleRoot, "app"),
		packageName: "cuenv",
		options:     `{"fields":["a b c"]}`,
	})
	assertContains(t, invalid, ErrorCodeInvalidInput)
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

func TestEmbedArgumentsAreSplitAndUnquotedAsCUEDoes(t *testing.T) {
	cases := map[string][]string{
		`glob=x/[a,b]*.json, allowEmptyGlob`: {"glob=x/[a,b]*.json", " allowEmptyGlob"},
		`file=#"x,y.json"#, type=json`:       {`file=#"x,y.json"#`, " type=json"},
		`glob="a,b/*.txt"`:                   {`glob="a,b/*.txt"`},
	}
	for body, want := range cases {
		got := splitAttributeArguments(body)
		if len(got) != len(want) {
			t.Fatalf("%s: got %q, want %q", body, got, want)
		}
		for index := range want {
			if got[index] != want[index] {
				t.Fatalf("%s: got %q, want %q", body, got, want)
			}
		}
	}
	for value, want := range map[string]string{
		`#"x.json"#`:    "x.json",
		`"a,b/*.txt"`:   "a,b/*.txt",
		`config/*.yaml`: "config/*.yaml",
	} {
		if got := unquoteAttributeValue(value); got != want {
			t.Fatalf("%s: got %q, want %q", value, got, want)
		}
	}
}
