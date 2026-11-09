#![allow(clippy::needless_pass_by_value)]

use super::{StepResult, TestWorld};
use cucumber::codegen::anyhow::anyhow;
use cucumber::{given, then};
use std::fmt::Write;

fn generate_env_cue(vars: &[(String, String)]) -> String {
    let mut cue = String::from(
        r#"package test

name: "env-test"

env: {
"#,
    );

    for (name, value) in vars {
        let _ = writeln!(cue, "    {name}: \"{value}\"");
    }

    cue.push_str("}\n");
    cue
}

#[given(expr = "a project with environment variables:")]
async fn given_project_with_env_vars(
    world: &mut TestWorld,
    step: &cucumber::gherkin::Step,
) -> StepResult {
    let table = step
        .table
        .as_ref()
        .ok_or_else(|| anyhow!("expected a data table for environment variables"))?;

    let vars: Vec<(String, String)> = table
        .rows
        .iter()
        .skip(1)
        .map(|row| (row[0].clone(), row[1].clone()))
        .collect();

    let cue_content = generate_env_cue(&vars);
    world.write_test_project("env_test", &cue_content).await
}

#[given(expr = "a project with no environment variables")]
async fn given_project_with_no_env_vars(world: &mut TestWorld) -> StepResult {
    // Schema-free for test isolation
    let cue_content = r#"package test

name: "empty-env-test"

env: {}
"#;

    world
        .write_test_project("empty_env_test", cue_content)
        .await
}

#[given(expr = "a project with a missing required environment variable")]
async fn given_project_with_missing_required_env(world: &mut TestWorld) -> StepResult {
    world
        .write_test_project(
            "missing_env_test",
            r#"package test

name: "missing-env-test"
env: { API_KEY!: string }
"#,
        )
        .await
}

#[given(expr = "a project with base environment {string}")]
async fn given_project_with_base_env(world: &mut TestWorld, base_env: String) -> StepResult {
    let (var_name, var_value) = base_env
        .split_once('=')
        .ok_or_else(|| anyhow!("expected a base environment entry in NAME=value form"))?;

    // Schema-free for test isolation
    let cue_content = format!(
        r#"package test

name: "env-inheritance-test"

env: {{
    {var_name}: "{var_value}"
    environment: {{
        dev: {{
            // Will be filled in by next step
        }}
    }}
}}
"#
    );

    world
        .write_test_project("env_inherit_test", &cue_content)
        .await?;

    // Store the base var info for the next step
    world
        .env_vars
        .insert("_base_var".to_string(), var_name.to_string());
    world
        .env_vars
        .insert("_base_value".to_string(), var_value.to_string());
    Ok(())
}

#[given(expr = "a derived environment {string} with {string}")]
async fn given_derived_environment(
    world: &mut TestWorld,
    env_name: String,
    env_var: String,
) -> StepResult {
    let (var_name, var_value) = env_var
        .split_once('=')
        .ok_or_else(|| anyhow!("expected a derived environment entry in NAME=value form"))?;

    let base_var = world
        .env_vars
        .get("_base_var")
        .ok_or_else(|| anyhow!("base environment name was not configured"))?;
    let base_value = world
        .env_vars
        .get("_base_value")
        .ok_or_else(|| anyhow!("base environment value was not configured"))?;

    // Schema-free for test isolation
    let cue_content = format!(
        r#"package test

name: "env-inheritance-test"

env: {{
    {base_var}: "{base_value}"
    environment: {{
        {env_name}: {{
            {var_name}: "{var_value}"
        }}
    }}
}}
"#
    );

    world.write_current_env_cue(&cue_content).await
}

#[then(expr = "the output should be valid JSON")]
fn then_output_is_valid_json(world: &mut TestWorld) {
    let result: Result<serde_json::Value, _> = serde_json::from_str(&world.last_output);
    assert!(
        result.is_ok(),
        "Output should be valid JSON. Actual output: {}",
        world.last_output
    );
}

// =============================================================================
// Error Handling Step Definitions
// =============================================================================

#[given(expr = "a project with invalid CUE syntax")]
async fn given_project_with_invalid_cue(world: &mut TestWorld) -> StepResult {
    // Create a CUE file with intentionally broken syntax (schema-free for test isolation)
    let cue_content = r#"package test

name: "invalid-syntax-test"

// Missing closing brace and invalid syntax
env: {
    BROKEN: "this is broken
    UNCLOSED: {
"#;

    world
        .write_test_project("invalid_cue_test", cue_content)
        .await
}

#[given(expr = "a project with no tasks or environment")]
async fn given_project_with_no_tasks_or_env(world: &mut TestWorld) -> StepResult {
    // Schema-free for test isolation
    let cue_content = r#"package test

name: "empty-project"
"#;

    world
        .write_test_project("empty_project_test", cue_content)
        .await
}
