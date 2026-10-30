//! Evaluate `.rules.cue` independently while preserving its module imports.

use std::path::Path;

use cuenv_core::Result;
use cuenv_core::manifest::DirectoryRules;

/// Evaluate a single rules file in its original module.
///
/// # Errors
///
/// Returns an error when the file, its imports, or its schema are invalid, or
/// when its evaluated contents do not match the rules manifest.
pub fn evaluate_rules_file(file_path: &Path) -> Result<DirectoryRules> {
    let value = cuengine::evaluate_cue_file(file_path).map_err(|error| {
        cuenv_core::Error::configuration(format!(
            "Failed to evaluate .rules.cue at {}: {error}",
            file_path.display()
        ))
    })?;
    serde_json::from_value(value).map_err(|error| {
        cuenv_core::Error::configuration(format!(
            "Failed to parse .rules.cue at {}: {error}",
            file_path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::evaluate_rules_file;

    #[test]
    fn evaluates_imports_and_schema_without_loading_the_project()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir_all(root.path().join("cue.mod"))?;
        std::fs::create_dir_all(root.path().join("rules"))?;
        std::fs::write(
            root.path().join("cue.mod/module.cue"),
            "module: \"example.org/rules@v0\"\nlanguage: version: \"v0.14.0\"\n",
        )?;
        std::fs::write(
            root.path().join("rules/schema.cue"),
            "package rules\n#DirectoryRules: {ignore: git: [\"target/\"]}\n",
        )?;
        std::fs::write(
            root.path().join("env.cue"),
            "package cuenv\nignore: git: [\"other/\"]\n",
        )?;
        let file = root.path().join(".rules.cue");
        std::fs::write(
            &file,
            "package cuenv\nimport \"example.org/rules/rules\"\nrules.#DirectoryRules\n",
        )?;
        let result = evaluate_rules_file(&file)?;
        assert_eq!(
            result
                .ignore
                .as_ref()
                .and_then(|ignore| ignore.get("git"))
                .map(|entry| entry.patterns()),
            Some(["target/".to_string()].as_slice())
        );
        std::fs::write(
            &file,
            "package cuenv\nimport \"example.org/rules/rules\"\nrules.#DirectoryRules & {ignore: git: [\"wrong/\"]}\n",
        )?;
        let failure = evaluate_rules_file(&file).expect_err("schema mismatch must fail");
        assert!(failure.to_string().contains(".rules.cue"));
        Ok(())
    }
}
