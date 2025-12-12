#![allow(missing_docs)]

use cuengine::evaluate_cue_package_typed;
use cuenv_core::manifest::{Base, Cuenv};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should resolve")
}

fn write_local_cuenv_module(root: &Path) {
    fs::create_dir_all(root.join("cue.mod")).unwrap();
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )
    .unwrap();

    // Copy the real schema package into the temporary module so imports work.
    let schema_src = repo_root().join("schema");
    let schema_dst = root.join("schema");
    fs::create_dir_all(&schema_dst).unwrap();
    for entry in fs::read_dir(&schema_src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("cue") {
            continue;
        }
        let file_name = path.file_name().unwrap();
        fs::copy(&path, schema_dst.join(file_name)).unwrap();
    }
}

#[test]
fn project_name_is_required_by_schema() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_local_cuenv_module(root);

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  // name intentionally omitted
}
"#,
    )
    .unwrap();

    let res = evaluate_cue_package_typed::<Cuenv>(root, "cuenv");
    assert!(res.is_err(), "schema should reject missing `name`");
}

#[test]
fn base_can_be_composed_standalone() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_local_cuenv_module(root);

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Base & {
  env: {
    HELLO: "world"
  }
}
"#,
    )
    .unwrap();

    let base = evaluate_cue_package_typed::<Base>(root, "cuenv").expect("Base should evaluate");
    assert!(base.env.is_some());
}
