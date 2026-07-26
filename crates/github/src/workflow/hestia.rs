//! Hestia cache maintenance workflow construction.

use crate::workflow::schema::{
    Concurrency, Job, PermissionLevel, Permissions, RunsOn, ScheduleTrigger, Step, Workflow,
    WorkflowDispatchTrigger, WorkflowInput, WorkflowTriggers,
};
use indexmap::IndexMap;

/// Immutable Hestia v2.0.0 action revision.
pub const HESTIA_ACTION: &str = "Mic92/hestia@fb239a2f72d4b6e26eec5425f289dea23b27a527";

/// Filename used for the repository-wide Hestia maintenance workflow.
pub const HESTIA_GC_WORKFLOW_FILENAME: &str = "cuenv-hestia-cache-gc.yml";

/// Build the repository-wide daily Hestia garbage-collection workflow.
///
/// Hestia cache storage is shared by every workflow in a repository, so one
/// default-branch GC job is sufficient regardless of how many pipelines use
/// the cache. GC runs are queued because overlapping repacks are unsafe.
#[must_use]
pub fn build_hestia_gc_workflow() -> Workflow {
    let mut dispatch_inputs = IndexMap::new();
    dispatch_inputs.insert(
        "dry-run".to_string(),
        WorkflowInput {
            description: "Plan only; do not repack, touch, or delete anything.".to_string(),
            required: None,
            default: Some("false".to_string()),
            input_type: Some("boolean".to_string()),
            options: None,
        },
    );

    let install_nix = Step::uses("DeterminateSystems/determinate-nix-action@v3")
        .with_name("Install Determinate Nix")
        .with_input(
            "extra-conf",
            serde_yaml::Value::String("accept-flake-config = true".to_string()),
        );
    let install_hestia = Step::uses(HESTIA_ACTION)
        .with_name("Install Hestia")
        .with_input("version", serde_yaml::Value::String("v2.0.0".to_string()));
    let run_gc = Step::run(
        r#""$HESTIA_BIN" gc \
  ${{ inputs.dry-run && '--dry-run' || '' }}"#,
    )
    .with_name("Run garbage collection")
    .with_env("GITHUB_TOKEN", "${{ github.token }}");

    let gc_job = Job {
        name: Some("Hestia Cache GC".to_string()),
        runs_on: RunsOn::Label("ubuntu-latest".to_string()),
        needs: Vec::new(),
        if_condition: Some(
            "github.event_name == 'schedule' || github.ref_name == github.event.repository.default_branch"
                .to_string(),
        ),
        strategy: None,
        environment: None,
        env: IndexMap::new(),
        concurrency: None,
        continue_on_error: None,
        timeout_minutes: Some(30),
        steps: vec![install_nix, install_hestia, run_gc],
    };

    Workflow {
        name: "Hestia Cache GC".to_string(),
        on: WorkflowTriggers {
            schedule: Some(vec![ScheduleTrigger {
                cron: "23 3 * * *".to_string(),
            }]),
            workflow_dispatch: Some(WorkflowDispatchTrigger {
                inputs: dispatch_inputs,
            }),
            ..Default::default()
        },
        concurrency: Some(Concurrency {
            group: "hestia-gc".to_string(),
            cancel_in_progress: Some(false),
        }),
        permissions: Some(Permissions {
            actions: Some(PermissionLevel::Write),
            contents: Some(PermissionLevel::Read),
            ..Default::default()
        }),
        env: IndexMap::new(),
        jobs: IndexMap::from([("gc".to_string(), gc_job)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hestia_gc_workflow_is_default_branch_only_and_non_overlapping() {
        let workflow = build_hestia_gc_workflow();
        let yaml = workflow.to_yaml().expect("GC workflow should serialize");

        assert!(yaml.contains("cron: 23 3 * * *"));
        assert!(yaml.contains("group: hestia-gc"));
        assert!(yaml.contains("cancel-in-progress: false"));
        assert!(yaml.contains("actions: write"));
        assert!(yaml.contains("github.event.repository.default_branch"));
        assert!(yaml.contains(HESTIA_ACTION));
        assert!(yaml.contains("version: v2.0.0"));
        assert!(!yaml.contains("nscloud-cache-action"));
    }
}
