//! GitHub workflows for `infrastructure.workflows.github`.
//!
//! A project that sets `infrastructure.workflows.github` gets two workflows:
//! one that plans on pull requests (and comments the plan) and one that
//! applies on `apply.on`. Both use the thin workflow layout, so the
//! project's `ci` contributors install cuenv and set up secret providers
//! before the main step runs.

use super::ProjectInfo;
use cuenv_core::Result;
use cuenv_core::ci::{CI, ManualTrigger, Pipeline, PipelineCondition, StringOrVec};
use cuenv_core::manifest::{
    InfrastructureApplyTrigger, InfrastructureApplyWorkflow, InfrastructureGitHubWorkflows,
    InfrastructureWorkflows, Project,
};
use cuenv_github::GitHubConfigExt;
use cuenv_github::workflow::schema::Step;
use cuenv_github::workflow::{CommandWorkflow, GitHubActionsEmitter};

/// The CI provider infrastructure workflows are emitted for.
pub(in crate::commands::sync::functions) const PROVIDER: &str = "github";

/// Where the plan step writes the plan for the comment step.
const PLAN_FILE: &str = "${{ runner.temp }}/cuenv-infrastructure-plan.txt";

/// The id of the plan step, read by the comment step.
const PLAN_STEP_ID: &str = "plan";

/// The project's `infrastructure.workflows.github`, when it sets one.
///
/// Decodes only `workflows`: ordinary commands must tolerate the rest of an
/// infrastructure configuration not decoding strictly.
///
/// # Errors
///
/// Returns a configuration error when `infrastructure.workflows` is invalid.
pub(in crate::commands::sync::functions) fn github_workflows(
    project: &Project,
) -> Result<Option<InfrastructureGitHubWorkflows>> {
    let Some(workflows) = project
        .infrastructure
        .as_ref()
        .and_then(|infrastructure| infrastructure.get("workflows"))
    else {
        return Ok(None);
    };
    let workflows: InfrastructureWorkflows =
        serde_json::from_value(workflows.clone()).map_err(|error| {
            cuenv_core::Error::configuration(format!(
                "Invalid infrastructure.workflows in project '{}': {error}",
                project.name
            ))
        })?;
    Ok(workflows.github)
}

/// Generate the plan and apply workflows for a project, or nothing when the
/// project does not set `infrastructure.workflows.github`.
pub(super) fn generate_workflows(project: &ProjectInfo) -> Result<Vec<(String, String)>> {
    let Some(workflows) = github_workflows(&project.config)? else {
        return Ok(Vec::new());
    };
    let project_path = if project.relative_path.as_os_str().is_empty() {
        None
    } else {
        Some(project.relative_path.to_string_lossy().to_string())
    };
    let path = project_path.as_deref().unwrap_or(".");

    let plan_command = plan_command(path, &project.config.name);
    let comment_steps: Vec<Step> = if workflows.plan.comment {
        vec![plan_comment_step(&project.config.name)]
    } else {
        Vec::new()
    };
    let plan = generate_workflow(
        project,
        project_path.as_deref(),
        "infrastructure-plan",
        plan_pipeline(),
        &CommandWorkflow {
            step_name: "Plan infrastructure",
            step_id: Some(PLAN_STEP_ID),
            command: &plan_command,
            cancel_in_progress: true,
            follow_up_steps: &comment_steps,
            job_environment: None,
        },
    )?;

    let apply_command = format!("cuenv infrastructure apply --yes --path {path}");
    let apply = generate_workflow(
        project,
        project_path.as_deref(),
        "infrastructure-apply",
        apply_pipeline(&workflows.apply),
        &CommandWorkflow {
            step_name: "Apply infrastructure",
            step_id: None,
            command: &apply_command,
            // A cancelled apply can leave the state lock held and resources
            // half converged, so newer runs queue behind it instead.
            cancel_in_progress: false,
            follow_up_steps: &[],
            job_environment: workflows.apply.environment.as_deref(),
        },
    )?;

    Ok(vec![plan, apply])
}

fn plan_pipeline() -> Pipeline {
    Pipeline {
        when: Some(PipelineCondition {
            pull_request: Some(true),
            ..PipelineCondition::default()
        }),
        derive_paths: Some(true),
        ..Pipeline::default()
    }
}

fn apply_pipeline(apply: &InfrastructureApplyWorkflow) -> Pipeline {
    let manual = (apply.manual || apply.on == InfrastructureApplyTrigger::Manual)
        .then_some(ManualTrigger::Enabled(true));
    let mut when = PipelineCondition {
        manual,
        ..PipelineCondition::default()
    };
    match apply.on {
        InfrastructureApplyTrigger::Push => {
            when.branch = Some(StringOrVec::String(apply.branch.clone()));
        }
        InfrastructureApplyTrigger::Tag => {
            when.tag = Some(StringOrVec::Vec(apply.tags.clone()));
        }
        InfrastructureApplyTrigger::Release => {
            when.release = Some(vec!["published".to_string()]);
        }
        InfrastructureApplyTrigger::Manual => {}
    }
    Pipeline {
        when: Some(when),
        derive_paths: Some(apply.on == InfrastructureApplyTrigger::Push),
        ..Pipeline::default()
    }
}

/// Run the plan, keep its output for the comment step and add it to the job
/// summary, then exit with the plan's status.
fn plan_command(path: &str, project_name: &str) -> String {
    format!(
        "set -o pipefail\n\
         status=0\n\
         cuenv infrastructure plan --path {path} 2>&1 | tee \"{PLAN_FILE}\" || status=$?\n\
         {{\n  \
           printf '### Infrastructure plan: `{project_name}`\\n\\n````diff\\n'\n  \
           cat \"{PLAN_FILE}\"\n  \
           printf '````\\n'\n\
         }} >> \"$GITHUB_STEP_SUMMARY\"\n\
         exit \"$status\""
    )
}

/// Create or edit the pull request comment that shows the plan. A hidden
/// marker per project finds the comment earlier runs posted.
fn plan_comment_step(project_name: &str) -> Step {
    let script = format!(
        r"const fs = require('fs');
const marker = '<!-- cuenv-infrastructure-plan:{project_name} -->';
const file = process.env.PLAN_FILE;
let plan = fs.existsSync(file) ? fs.readFileSync(file, 'utf8').trimEnd() : '';
if (plan === '') plan = 'The plan produced no output.';
const limit = 60000;
if (plan.length > limit) plan = `${{plan.slice(0, limit)}}\n… (truncated; the job log has the full plan)`;
const failed = process.env.PLAN_OUTCOME !== 'success';
const run = `${{context.serverUrl}}/${{context.repo.owner}}/${{context.repo.repo}}/actions/runs/${{context.runId}}`;
const body = [
  marker,
  `### ${{failed ? '❌ Infrastructure plan failed' : 'Infrastructure plan'}}: \`{project_name}\``,
  '',
  '````diff',
  plan,
  '````',
  '',
  `[Workflow run](${{run}}) · ${{context.sha.slice(0, 7)}}`,
].join('\n');
const {{ owner, repo }} = context.repo;
const issue_number = context.issue.number;
const comments = await github.paginate(github.rest.issues.listComments, {{ owner, repo, issue_number, per_page: 100 }});
const existing = comments.find((comment) => comment.body?.includes(marker));
if (existing) {{
  await github.rest.issues.updateComment({{ owner, repo, comment_id: existing.id, body }});
}} else {{
  await github.rest.issues.createComment({{ owner, repo, issue_number, body }});
}}"
    );
    Step::uses("actions/github-script@v7")
        .with_name("Comment infrastructure plan")
        .with_if(format!(
            "always() && steps.{PLAN_STEP_ID}.outcome != 'skipped' && \
             github.event.pull_request.head.repo.full_name == github.repository"
        ))
        .with_env("PLAN_FILE", PLAN_FILE)
        .with_env(
            "PLAN_OUTCOME",
            format!("${{{{ steps.{PLAN_STEP_ID}.outcome }}}}"),
        )
        .with_input("script", serde_yaml::Value::String(script))
}

fn generate_workflow(
    project: &ProjectInfo,
    project_path: Option<&str>,
    pipeline_name: &str,
    pipeline: Pipeline,
    command: &CommandWorkflow<'_>,
) -> Result<(String, String)> {
    use cuenv_ci::compiler::{Compiler, CompilerOptions};

    // The compiler derives triggers only for projects with a `ci` block.
    let mut config = project.config.clone();
    let github_config = config
        .ci
        .get_or_insert_with(CI::default)
        .github_config_for_pipeline(pipeline_name);

    let options = CompilerOptions {
        pipeline_name: Some(pipeline_name.to_string()),
        pipeline: Some(pipeline),
        ci_mode: true,
        module_root: Some(project.module_root.clone()),
        project_path: project_path.map(str::to_string),
        ..Default::default()
    };
    let mut ir = Compiler::with_options(config, options)
        .compile()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to compile project: {e}")))?;
    // Only the contributor phase steps run; the main step replaces the tasks.
    ir.tasks.retain(|task| task.phase.is_some());
    ir.pipeline.name = pipeline_name.to_string();
    ir.pipeline.project_name = Some(project.config.name.clone());
    ir.pipeline.project_path = project_path.map(str::to_string);

    GitHubActionsEmitter::from_config(&github_config)
        .with_nix()
        .emit_command_workflow(&ir, command)
        .map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to emit infrastructure workflow: {e}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn project(workflows: &serde_json::Value) -> ProjectInfo {
        let mut config = Project::new("dns");
        config.infrastructure = Some(serde_json::json!({
            "state": {"turso": {"url": "http://127.0.0.1:8080"}},
            "workflows": workflows,
        }));
        ProjectInfo {
            project_path: PathBuf::from("/repo/infrastructure/dns"),
            relative_path: PathBuf::from("infrastructure/dns"),
            module_root: PathBuf::from("/repo"),
            config,
        }
    }

    fn workflows(
        workflows: &serde_json::Value,
    ) -> std::result::Result<Vec<serde_yaml::Value>, Box<dyn std::error::Error>> {
        let generated = generate_workflows(&project(workflows))?;
        let names: Vec<&str> = generated.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "dns-infrastructure-plan.yml",
                "dns-infrastructure-apply.yml"
            ]
        );
        generated
            .iter()
            .map(|(_, yaml)| Ok(serde_yaml::from_str(yaml)?))
            .collect()
    }

    fn steps(workflow: &serde_yaml::Value, job: &str) -> Vec<serde_yaml::Value> {
        workflow["jobs"][job]["steps"]
            .as_sequence()
            .cloned()
            .unwrap_or_default()
    }

    fn step<'a>(steps: &'a [serde_yaml::Value], name: &str) -> Option<&'a serde_yaml::Value> {
        steps
            .iter()
            .find(|step| step["name"].as_str() == Some(name))
    }

    #[test]
    fn projects_without_workflows_generate_nothing()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut info = project(&serde_json::Value::Null);
        info.config.infrastructure = Some(serde_json::json!({"state": {}}));
        assert!(generate_workflows(&info)?.is_empty());
        info.config.infrastructure = None;
        assert!(generate_workflows(&info)?.is_empty());
        Ok(())
    }

    #[test]
    fn invalid_workflows_are_a_configuration_error() {
        let info = project(&serde_json::json!({"github": {"apply": {"on": "nightly"}}}));
        let error = generate_workflows(&info).expect_err("invalid apply.on");
        assert!(
            error.to_string().contains("infrastructure.workflows"),
            "{error}"
        );
    }

    #[test]
    fn defaults_plan_and_comment_on_pull_requests_and_apply_on_main()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({"github": {}}))?;
        let (plan, apply) = (&generated[0], &generated[1]);

        assert_eq!(
            plan["on"]["pull_request"]["paths"][2],
            "infrastructure/dns/**"
        );
        assert!(plan["on"].get("push").is_none());
        assert_eq!(plan["concurrency"]["cancel-in-progress"], true);
        let job = &plan["jobs"]["dns-infrastructure-plan"];
        assert_eq!(job["permissions"]["pull-requests"], "write");
        let plan_steps = steps(plan, "dns-infrastructure-plan");
        let plan_step = step(&plan_steps, "Plan infrastructure").ok_or("plan step")?;
        assert_eq!(plan_step["id"], "plan");
        let run = plan_step["run"].as_str().ok_or("plan run")?;
        assert!(run.starts_with("set -o pipefail\n"), "{run}");
        assert!(
            run.contains("cuenv infrastructure plan --path infrastructure/dns 2>&1 | tee"),
            "{run}"
        );
        assert!(run.contains("$GITHUB_STEP_SUMMARY"), "{run}");
        let comment = step(&plan_steps, "Comment infrastructure plan").ok_or("comment step")?;
        assert_eq!(comment["uses"], "actions/github-script@v7");
        assert_eq!(comment["env"]["PLAN_OUTCOME"], "${{ steps.plan.outcome }}");
        let script = comment["with"]["script"].as_str().ok_or("script")?;
        assert!(
            script.contains("<!-- cuenv-infrastructure-plan:dns -->"),
            "{script}"
        );
        assert!(script.contains("updateComment"), "{script}");

        assert_eq!(apply["on"]["push"]["branches"][0], "main");
        assert!(apply["on"].get("workflow_dispatch").is_some());
        assert!(apply["on"].get("pull_request").is_none());
        assert_eq!(apply["concurrency"]["cancel-in-progress"], false);
        let apply_steps = steps(apply, "dns-infrastructure-apply");
        let apply_step = step(&apply_steps, "Apply infrastructure").ok_or("apply step")?;
        assert_eq!(
            apply_step["run"],
            "cuenv infrastructure apply --yes --path infrastructure/dns"
        );
        assert!(step(&apply_steps, "Comment infrastructure plan").is_none());
        Ok(())
    }

    #[test]
    fn plan_comment_can_be_disabled() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({"github": {"plan": {"comment": false}}}))?;
        let plan_steps = steps(&generated[0], "dns-infrastructure-plan");
        assert!(step(&plan_steps, "Plan infrastructure").is_some());
        assert!(step(&plan_steps, "Comment infrastructure plan").is_none());
        Ok(())
    }

    #[test]
    fn apply_on_tag_triggers_on_tag_pushes() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let generated = workflows(&serde_json::json!({
            "github": {"apply": {"on": "tag", "tags": ["dns-*"], "manual": false}}
        }))?;
        let on = &generated[1]["on"];
        assert_eq!(on["push"]["tags"][0], "dns-*");
        assert!(on["push"].get("branches").is_none());
        assert!(on.get("workflow_dispatch").is_none());
        Ok(())
    }

    #[test]
    fn apply_on_release_triggers_on_published_releases()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({
            "github": {"apply": {"on": "release", "environment": "production"}}
        }))?;
        let apply = &generated[1];
        assert_eq!(apply["on"]["release"]["types"][0], "published");
        assert!(apply["on"].get("push").is_none());
        assert!(apply["on"].get("workflow_dispatch").is_some());
        let job = &apply["jobs"]["dns-infrastructure-apply"];
        assert_eq!(job["environment"], "production");
        let apply_steps = steps(apply, "dns-infrastructure-apply");
        let apply_step = step(&apply_steps, "Apply infrastructure").ok_or("apply step")?;
        assert!(apply_step["env"].get("CUENV_ENVIRONMENT").is_none());
        Ok(())
    }

    #[test]
    fn apply_on_manual_runs_only_manually() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({
            "github": {"apply": {"on": "manual", "manual": false}}
        }))?;
        let on = &generated[1]["on"];
        assert!(on.get("workflow_dispatch").is_some());
        assert!(on.get("push").is_none());
        assert!(on.get("release").is_none());
        Ok(())
    }
}
