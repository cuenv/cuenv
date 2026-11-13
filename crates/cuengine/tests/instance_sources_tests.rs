//! What `instance_sources_with` reports about the packages an instance
//! imports.

use cuengine::{SourcesRequest, instance_sources, instance_sources_with};
use std::error::Error;
use std::fs;
use std::path::Path;

type TestResult = Result<(), Box<dyn Error>>;

fn write(root: &Path, relative: &str, content: &str) -> TestResult {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

/// A module whose `app` imports `content`, whose root is in error, and a
/// child directory of `content` that inherits its `name`.
fn module() -> Result<tempfile::TempDir, Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    write(
        root,
        "cue.mod/module.cue",
        "module: \"example.com/refs@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    write(
        root,
        "content/env.cue",
        "package cuenv\n\nname: \"content\"\nbroken: 1\nbroken: 2\n",
    )?;
    write(
        root,
        "content/drafts/env.cue",
        "package cuenv\n\ndrafts: true\n",
    )?;
    write(
        root,
        "app/env.cue",
        "package cuenv\n\nimport (\n\tcontent \"example.com/refs/content:cuenv\"\n\tdrafts \
         \"example.com/refs/content/drafts:cuenv\"\n)\n\nname: \"app\"\nuses: [content.name, \
         drafts.drafts]\n",
    )?;
    Ok(directory)
}

#[test]
fn every_imported_package_is_described_with_the_requested_fields() -> TestResult {
    let directory = module()?;
    let root = directory.path();
    let fields = ["name".to_string()];

    let sources = instance_sources_with(
        root,
        &root.join("app"),
        SourcesRequest {
            package_name: "cuenv",
            fields: &fields,
        },
    )?;

    assert_eq!(sources.module, "example.com/refs@v0");
    assert_eq!(sources.imports.len(), 2, "{:?}", sources.imports);
    let imported = |path: &str| {
        sources
            .imports
            .iter()
            .find(|imported| imported.import_path == path)
            .ok_or_else(|| format!("no import {path} in {:?}", sources.imports))
    };
    let content = imported("example.com/refs/content:cuenv")?;
    assert_eq!(content.import_path, "example.com/refs/content:cuenv");
    assert_eq!(content.directory, "content");
    assert_eq!(content.package, "cuenv");
    assert_eq!(content.module, "example.com/refs@v0");
    assert_eq!(content.imported_by, "");
    assert_eq!(
        content.fields.get("name"),
        Some(&serde_json::json!("content")),
        "a root in error still yields its name"
    );
    let drafts = imported("example.com/refs/content/drafts:cuenv")?;
    assert_eq!(drafts.directory, "content/drafts");
    assert_eq!(
        drafts.fields.get("name"),
        Some(&serde_json::json!("content"))
    );
    assert_eq!(drafts.ancestors.len(), 1, "{:?}", drafts.ancestors);
    assert_eq!(drafts.ancestors[0].directory, "content");
    assert_eq!(
        drafts.ancestors[0].fields.get("name"),
        Some(&serde_json::json!("content"))
    );
    Ok(())
}

#[test]
fn without_fields_no_package_is_built_or_listed() -> TestResult {
    let directory = module()?;
    let root = directory.path();

    let sources = instance_sources(root, &root.join("app"), "cuenv")?;

    assert!(sources.imports.is_empty());
    assert!(
        sources.files.iter().any(|file| file == "content/env.cue"),
        "{:?}",
        sources.files
    );
    Ok(())
}
