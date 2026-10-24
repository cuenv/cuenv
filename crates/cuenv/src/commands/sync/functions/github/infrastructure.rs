//! GitHub workflows for `infrastructure.workflows`.
//!
//! A project with `github` in `ci.providers` that sets
//! `infrastructure.workflows` gets two workflows:
//! one that plans on pull requests (and comments the plan) and one that
//! applies on `apply.on`. Both use the thin workflow layout, so the
//! project's `ci` contributors install cuenv and set up secret providers
//! before the main step runs.

use super::ProjectInfo;
use cuenv_core::Result;
use cuenv_core::ci::{CI, ManualTrigger, Pipeline, PipelineCondition, StringOrVec};
use cuenv_core::manifest::{
    InfrastructureApplyTrigger, InfrastructureApplyWorkflow, InfrastructureWorkflows, Project,
};
use cuenv_github::GitHubConfigExt;
use cuenv_github::workflow::schema::Step;
use cuenv_github::workflow::{CommandWorkflow, GitHubActionsEmitter};

/// Pipeline names of the plan and apply workflows; the workflow files are
/// `<project>-<pipeline>.yml`.
const PLAN_PIPELINE: &str = "infrastructure-plan";
const APPLY_PIPELINE: &str = "infrastructure-apply";

/// Where the plan step writes the plan for the comment step.
const PLAN_FILE: &str = "${{ runner.temp }}/cuenv-infrastructure-plan.txt";

/// The id of the plan step, read by the comment step.
const PLAN_STEP_ID: &str = "plan";

/// The project's `infrastructure.workflows`, when it sets them.
///
/// Decodes only `workflows`: ordinary commands must tolerate the rest of an
/// infrastructure configuration not decoding strictly.
///
/// # Errors
///
/// Returns a configuration error when `infrastructure.workflows` is invalid.
fn workflows(project: &Project) -> Result<Option<InfrastructureWorkflows>> {
    let Some(workflows) = project
        .infrastructure
        .as_ref()
        .and_then(|infrastructure| infrastructure.get("workflows"))
    else {
        return Ok(None);
    };
    serde_json::from_value(workflows.clone())
        .map(Some)
        .map_err(|error| {
            cuenv_core::Error::configuration(format!(
                "Invalid infrastructure.workflows in project '{}': {error}",
                project.name
            ))
        })
}

/// Workflow files of a project with infrastructure but without
/// `infrastructure.workflows`, which an earlier sync may have written.
pub(super) fn stale_workflows(project: &ProjectInfo) -> Result<Vec<String>> {
    if project.config.infrastructure.is_none() || workflows(&project.config)?.is_some() {
        return Ok(Vec::new());
    }
    Ok([PLAN_PIPELINE, APPLY_PIPELINE]
        .iter()
        .map(|pipeline| workflow_filename(&project.config.name, pipeline))
        .collect())
}

fn workflow_filename(project_name: &str, pipeline_name: &str) -> String {
    format!(
        "{}.yml",
        super::sanitize_workflow_name(&format!("{project_name}-{pipeline_name}"))
    )
}

/// Generate the plan and apply workflows for a project, or nothing when the
/// project does not set `infrastructure.workflows`.
pub(super) fn generate_workflows(project: &ProjectInfo) -> Result<Vec<(String, String)>> {
    let Some(workflows) = workflows(&project.config)? else {
        return Ok(Vec::new());
    };
    if let Some(ci) = &project.config.ci {
        for name in [PLAN_PIPELINE, APPLY_PIPELINE] {
            if ci.pipelines.contains_key(name) {
                return Err(cuenv_core::Error::configuration(format!(
                    "Project '{}' defines a ci pipeline named '{name}', whose workflow file \
                     infrastructure.workflows also writes. Rename the pipeline.",
                    project.config.name
                )));
            }
        }
    }
    let project_path = if project.relative_path.as_os_str().is_empty() {
        None
    } else {
        Some(project.relative_path.to_string_lossy().to_string())
    };
    let path = shell_quote(project_path.as_deref().unwrap_or("."));

    let plan_command = plan_command(&path, &project.config.name);
    let comment_steps: Vec<Step> = if workflows.plan.comment {
        vec![plan_comment_step(&project.config.name)]
    } else {
        Vec::new()
    };
    let plan = generate_workflow(&WorkflowRequest {
        project,
        project_path: project_path.as_deref(),
        pipeline_name: PLAN_PIPELINE,
        pipeline: plan_pipeline(),
        command: &CommandWorkflow {
            step_name: "Plan infrastructure",
            step_id: Some(PLAN_STEP_ID),
            command: &plan_command,
            concurrency_group: None,
            cancel_in_progress: true,
            job_if: None,
            follow_up_steps: &comment_steps,
            job_environment: None,
        },
    })?;

    let apply_command = format!("cuenv infrastructure apply --yes --path {path}");
    // Pushes and manual runs apply only the configured branch.
    let apply_if = (workflows.apply.on == InfrastructureApplyTrigger::Push).then(|| {
        // GitHub expression strings escape a quote by doubling it.
        let branch = workflows.apply.branch.replace('\'', "''");
        format!("github.ref == 'refs/heads/{branch}'")
    });
    let apply = generate_workflow(&WorkflowRequest {
        project,
        project_path: project_path.as_deref(),
        pipeline_name: APPLY_PIPELINE,
        pipeline: apply_pipeline(&workflows.apply),
        command: &CommandWorkflow {
            step_name: "Apply infrastructure",
            step_id: None,
            command: &apply_command,
            // One group for every ref: a cancelled apply can leave the state
            // lock held and resources half converged, so every apply queues
            // behind the one in progress.
            concurrency_group: Some("${{ github.workflow }}"),
            cancel_in_progress: false,
            job_if: apply_if.as_deref(),
            follow_up_steps: &[],
            job_environment: workflows.apply.environment.as_deref(),
        },
    })?;

    Ok(vec![plan, apply])
}

/// Quote a value for a POSIX shell command line.
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
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
/// marker per project finds the comment an earlier run posted; only comments
/// by the workflow's own token count, so nobody else's comment is edited.
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
const existing = comments.find((comment) =>
  comment.user?.login === 'github-actions[bot]' && comment.body?.includes(marker));
if (existing) {{
  await github.rest.issues.updateComment({{ owner, repo, comment_id: existing.id, body }});
}} else {{
  await github.rest.issues.createComment({{ owner, repo, issue_number, body }});
}}"
    );
    // `!cancelled()`: a plan cancelled by a newer push must not report its
    // partial output as a failure.
    Step::uses("actions/github-script@v7")
        .with_name("Comment infrastructure plan")
        .with_if(format!(
            "!cancelled() && steps.{PLAN_STEP_ID}.outcome != 'skipped' && \
             github.event.pull_request.head.repo.full_name == github.repository"
        ))
        .with_env("PLAN_FILE", PLAN_FILE)
        .with_env(
            "PLAN_OUTCOME",
            format!("${{{{ steps.{PLAN_STEP_ID}.outcome }}}}"),
        )
        .with_input("script", serde_yaml::Value::String(script))
}

struct WorkflowRequest<'a> {
    project: &'a ProjectInfo,
    project_path: Option<&'a str>,
    pipeline_name: &'a str,
    pipeline: Pipeline,
    command: &'a CommandWorkflow<'a>,
}

fn generate_workflow(request: &WorkflowRequest<'_>) -> Result<(String, String)> {
    use cuenv_ci::compiler::{Compiler, CompilerOptions};

    let WorkflowRequest {
        project,
        project_path,
        pipeline_name,
        ref pipeline,
        command,
    } = *request;

    // The compiler derives triggers only for projects with a `ci` block.
    let mut config = project.config.clone();
    let github_config = config
        .ci
        .get_or_insert_with(CI::default)
        .github_config_for_pipeline(pipeline_name);

    let options = CompilerOptions {
        pipeline_name: Some(pipeline_name.to_string()),
        pipeline: Some(pipeline.clone()),
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
        let info = project(&serde_json::json!({"apply": {"on": "nightly"}}));
        let error = generate_workflows(&info).expect_err("invalid apply.on");
        assert!(
            error.to_string().contains("infrastructure.workflows"),
            "{error}"
        );
    }

    #[test]
    fn defaults_plan_and_comment_on_pull_requests_and_apply_on_main()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({}))?;
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
        let generated = workflows(&serde_json::json!({"plan": {"comment": false}}))?;
        let plan_steps = steps(&generated[0], "dns-infrastructure-plan");
        assert!(step(&plan_steps, "Plan infrastructure").is_some());
        assert!(step(&plan_steps, "Comment infrastructure plan").is_none());
        Ok(())
    }

    #[test]
    fn apply_on_tag_triggers_on_tag_pushes() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let generated = workflows(&serde_json::json!({
            "apply": {"on": "tag", "tags": ["dns-*"], "manual": false}
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
            "apply": {"on": "release", "environment": "production"}
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
            "apply": {"on": "manual", "manual": false}
        }))?;
        let on = &generated[1]["on"];
        assert!(on.get("workflow_dispatch").is_some());
        assert!(on.get("push").is_none());
        assert!(on.get("release").is_none());
        Ok(())
    }

    #[test]
    fn apply_on_push_runs_only_for_the_branch_and_queues_every_ref()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({"apply": {"branch": "it's"}}))?;
        let apply = &generated[1];
        assert_eq!(apply["concurrency"]["group"], "${{ github.workflow }}");
        assert_eq!(
            apply["jobs"]["dns-infrastructure-apply"]["if"],
            "github.ref == 'refs/heads/it''s'"
        );
        let plan = &generated[0];
        assert!(plan["jobs"]["dns-infrastructure-plan"].get("if").is_none());
        Ok(())
    }

    #[test]
    fn comment_skips_cancelled_runs_and_foreign_comments()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({}))?;
        let plan_steps = steps(&generated[0], "dns-infrastructure-plan");
        let comment = step(&plan_steps, "Comment infrastructure plan").ok_or("comment step")?;
        let condition = comment["if"].as_str().ok_or("condition")?;
        assert!(condition.starts_with("!cancelled() && "), "{condition}");
        assert!(
            condition
                .contains("github.event.pull_request.head.repo.full_name == github.repository"),
            "{condition}"
        );
        let script = comment["with"]["script"].as_str().ok_or("script")?;
        assert!(
            script.contains("comment.user?.login === 'github-actions[bot]'"),
            "{script}"
        );
        Ok(())
    }

    #[test]
    fn paths_that_need_quoting_are_quoted() {
        assert_eq!(shell_quote("infrastructure/dns"), "infrastructure/dns");
        assert_eq!(shell_quote("my infra"), "'my infra'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn a_pipeline_with_a_generated_name_is_refused() {
        let mut info = project(&serde_json::json!({}));
        let mut ci = CI::default();
        ci.pipelines
            .insert(APPLY_PIPELINE.to_string(), Pipeline::default());
        info.config.ci = Some(ci);
        let error = generate_workflows(&info).expect_err("name collision");
        assert!(
            error.to_string().contains("infrastructure-apply"),
            "{error}"
        );
    }

    #[test]
    fn disabling_workflows_marks_their_files_stale()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut info = project(&serde_json::json!({}));
        info.config.name = "rawkode.academy".to_string();
        assert!(stale_workflows(&info)?.is_empty());
        info.config.infrastructure = Some(serde_json::json!({"state": {}}));
        assert_eq!(
            stale_workflows(&info)?,
            [
                "rawkodeacademy-infrastructure-plan.yml",
                "rawkodeacademy-infrastructure-apply.yml"
            ]
        );
        info.config.infrastructure = None;
        assert!(stale_workflows(&info)?.is_empty());
        Ok(())
    }
}
