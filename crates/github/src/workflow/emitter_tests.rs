use super::*;
use cuenv_ci::ir::{CachePolicy, PipelineMetadata, ResourceRequirements, Task, TriggerCondition};
use cuenv_core::ci::PipelineMode;
use std::collections::BTreeMap;

/// Create an IR for testing expanded mode behavior.
/// Uses PipelineMode::Expanded explicitly since tests check multi-job output.
fn make_ir(tasks: Vec<Task>) -> IntermediateRepresentation {
    IntermediateRepresentation {
        version: "1.4".to_string(),
        pipeline: PipelineMetadata {
            name: "test-pipeline".to_string(),
            mode: PipelineMode::Expanded,
            environment: None,
            requires_onepassword: false,
            project_name: None,
            project_path: None,
            trigger: None,
            pipeline_tasks: vec![],
            pipeline_task_defs: vec![],
        },
        runtimes: vec![],
        tasks,
    }
}

/// Helper to create a phase task for testing
fn make_phase_task(id: &str, command: &[&str], phase: BuildStage, priority: i32) -> Task {
    Task {
        id: id.to_string(),
        runtime: None,
        command: command.iter().map(|s| (*s).to_string()).collect(),
        shell: command.len() == 1,
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        resources: None,
        concurrency_group: None,
        inputs: vec![],
        outputs: vec![],
        depends_on: vec![],
        cache_policy: CachePolicy::Disabled,
        deployment: false,
        manual_approval: false,
        matrix: None,
        artifact_downloads: vec![],
        params: BTreeMap::new(),
        phase: Some(phase),
        label: None,
        priority: Some(priority),
        contributor: None,
        condition: None,
        provider_hints: None,
    }
}

fn make_task(id: &str, command: &[&str]) -> Task {
    Task {
        id: id.to_string(),
        runtime: None,
        command: command.iter().map(|s| (*s).to_string()).collect(),
        shell: false,
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        resources: None,
        concurrency_group: None,
        inputs: vec![],
        outputs: vec![],
        depends_on: vec![],
        cache_policy: CachePolicy::Normal,
        deployment: false,
        manual_approval: false,
        matrix: None,
        artifact_downloads: vec![],
        params: BTreeMap::new(),
        phase: None,
        label: None,
        priority: None,
        contributor: None,
        condition: None,
        provider_hints: None,
    }
}

fn assert_github_context_env(step: &Step) {
    assert_eq!(
        step.env.get("GITHUB_TOKEN"),
        Some(&"${{ secrets.GITHUB_TOKEN }}".to_string())
    );
    assert_eq!(
        step.env.get("GITHUB_ACTOR"),
        Some(&"${{ github.actor }}".to_string())
    );
    assert_eq!(
        step.env.get("GITHUB_REF_TYPE"),
        Some(&"${{ github.ref_type }}".to_string())
    );
    assert_eq!(
        step.env.get("GITHUB_REF_NAME"),
        Some(&"${{ github.ref_name }}".to_string())
    );
}

#[path = "emitter_tests/jobs.rs"]
mod jobs;
#[path = "emitter_tests/matrix.rs"]
mod matrix;
#[path = "emitter_tests/phases.rs"]
mod phases;
#[path = "emitter_tests/triggers.rs"]
mod triggers;
#[path = "emitter_tests/workflows.rs"]
mod workflows;
#[path = "emitter_tests/working_directory.rs"]
mod working_directory;
