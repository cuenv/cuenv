//! Infrastructure initialisation runs before project CUE evaluation.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

type TestResult = Result<(), Box<dyn Error>>;

fn module(root: &Path) -> TestResult {
    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"example.com/init@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    // This package cannot be evaluated until its provider types are restored.
    fs::write(
        root.join("env.cue"),
        "package cuenv\nimport missing \"registry.terraform.io/example/missing/resources/example_resource\"\nname: missing.name\n",
    )?;
    Ok(())
}

fn invoke(root: &Path, args: &[&str]) -> Result<Output, Box<dyn Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_cuenv"))
        .current_dir(root)
        .env("TF_PLUGIN_CACHE_DIR", root.join("plugin-cache"))
        .args(args)
        .output()?)
}

#[test]
fn init_aliases_bypass_project_evaluation_and_do_not_rewrite_lockfile() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    module(root)?;
    let lock = "version = 4\n";
    fs::write(root.join("cuenv.lock"), lock)?;
    let nested = root.join("nested");
    fs::create_dir(&nested)?;

    for alias in ["infrastructure", "infra", "i"] {
        let output = invoke(root, &[alias, "init", "--path", "nested"])?;
        assert!(
            output.status.success(),
            "{alias}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout)?;
        assert!(
            text.contains("No infrastructure providers are pinned"),
            "{text}"
        );
        assert_eq!(fs::read_to_string(root.join("cuenv.lock"))?, lock);
    }
    Ok(())
}

#[test]
fn init_requires_an_archive_pin_for_the_current_platform() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    module(root)?;
    let digest = format!("sha256:{}", "a".repeat(64));
    let lock = format!(
        "version = 5\n[infrastructure_providers.\"registry.terraform.io/hashicorp/random\"]\nversion = \"3.9.1\"\nschema_digest = \"{digest}\"\n[infrastructure_providers.\"registry.terraform.io/hashicorp/random\".platforms]\nfictional_arch = \"{digest}\"\n"
    );
    fs::write(root.join("cuenv.lock"), &lock)?;

    let output = invoke(root, &["infra", "init"])?;
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr)?;
    assert!(error.contains("is not published for"), "{error}");
    assert_eq!(fs::read_to_string(root.join("cuenv.lock"))?, lock);
    assert!(!root.join("cue.mod/gen").exists());

    // Plain sync must try the lockfile before evaluating the project, whose
    // missing import would otherwise mask this actionable pin error.
    let sync = invoke(root, &["sync"])?;
    assert!(!sync.status.success());
    let error = String::from_utf8(sync.stderr)?;
    assert!(error.contains("is not published for"), "{error}");
    assert!(!error.contains("cannot find package"), "{error}");
    Ok(())
}
