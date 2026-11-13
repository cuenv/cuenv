//! GitHub workflows for `infrastructure.workflows`.
//!
//! A project with `github` in `ci.providers` that sets
//! `infrastructure.workflows` gets two workflows:
//! one that plans on pull requests (and comments the plan) and one that
//! applies on `apply.on`. Both use the thin workflow layout, so the
//! project's `ci` contributors install cuenv and set up secret providers
//! before the main step runs.

use super::super::implementation::ProjectInfo;
use cuenv_core::Result;
use cuenv_core::ci::{CI, ManualTrigger, Pipeline, PipelineCondition, StringOrVec};
use cuenv_core::manifest::{
    InfrastructureApplyTrigger, InfrastructureApplyWorkflow, InfrastructureWorkflows, Project,
};
use cuenv_github::GitHubConfigExt;
use cuenv_github::workflow::schema::{PermissionLevel, Permissions, Step};
use cuenv_github::workflow::{CommandWorkflow, FollowUpJob, GitHubActionsEmitter, TokenScope};

/// Pipeline names of the plan and apply workflows; the workflow files are
/// `<project>-<pipeline>.yml`.
const PLAN_PIPELINE: &str = "infrastructure-plan";
const APPLY_PIPELINE: &str = "infrastructure-apply";

/// Where the plan step writes the plan for the comment step.
const PLAN_FILE: &str = "${{ runner.temp }}/cuenv-infrastructure-plan.txt";

/// The id of the plan step.
const PLAN_STEP_ID: &str = "plan";

/// The artifact that carries the plan from the plan job to the comment job.
const PLAN_ARTIFACT: &str = "cuenv-infrastructure-plan";

/// A pull request from a branch of this repository, not a fork.
const SAME_REPOSITORY: &str = "github.event.pull_request.head.repo.full_name == github.repository";

/// The id of the step that finds the reviewed plan digest.
const REVIEWED_PLAN_STEP_ID: &str = "reviewed";

/// The manual input that names the reviewed plan digest.
const PLAN_DIGEST_INPUT: &str = "plan_digest";

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

/// Export separate infrastructure leniently for workflow metadata and secret
/// contributor discovery. Resource values need not be concrete here.
fn infrastructure_configuration(
    project: &ProjectInfo,
    package: &str,
) -> Result<Option<serde_json::Value>> {
    let Some(locator) = load_locator(project, package)? else {
        return Ok(project.config.infrastructure.clone());
    };
    if project.config.infrastructure.is_some() {
        return Err(cuenv_core::Error::configuration(format!(
            "Project '{}' declares both inline infrastructure and .infrastructure.cue; choose one configuration",
            project.config.name
        )));
    }
    let options = cuengine::ModuleEvalOptions {
        target_dir: Some(locator.directory.to_string_lossy().into_owned()),
        export_paths: vec!["infrastructure".to_string()],
        instance_failures: cuengine::InstanceFailures::Fail,
        ..Default::default()
    };
    let evaluated =
        cuengine::evaluate_module(&project.module_root, &locator.package, Some(&options)).map_err(
            |error| {
                cuenv_core::Error::configuration(format!(
                    "Failed to read infrastructure workflows for project '{}': {error}",
                    project.config.name
                ))
            },
        )?;
    let value = evaluated.instances.values().next().ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "Infrastructure package '{}' was not found in {}",
            locator.package,
            locator.directory.display()
        ))
    })?;
    Ok(value.get("infrastructure").cloned())
}

fn project_workflows(
    project: &ProjectInfo,
    package: &str,
) -> Result<Option<InfrastructureWorkflows>> {
    let mut configuration = Project::new(&project.config.name);
    configuration.infrastructure = infrastructure_configuration(project, package)?;
    workflows(&configuration)
}

fn load_locator(
    project: &ProjectInfo,
    package: &str,
) -> Result<Option<crate::commands::infrastructure::locator::Locator>> {
    crate::commands::infrastructure::locator::load(&project.project_path)
        .map(|locator| locator.filter(|locator| locator.project_package == package))
        .map_err(|error| cuenv_core::Error::configuration(error.to_string()))
}

/// Workflow files of a project with infrastructure but without
/// `infrastructure.workflows`, which an earlier sync may have written.
pub(super) fn stale_workflows(project: &ProjectInfo, package: &str) -> Result<Vec<String>> {
    if (project.config.infrastructure.is_none() && load_locator(project, package)?.is_none())
        || project_workflows(project, package)?.is_some()
    {
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
        super::implementation::sanitize_workflow_name(&format!("{project_name}-{pipeline_name}"))
    )
}

/// Generate the plan and apply workflows for a project, or nothing when the
/// project does not set `infrastructure.workflows`.
pub(super) fn generate_workflows(
    project: &ProjectInfo,
    package: &str,
) -> Result<Vec<(String, String)>> {
    let Some(workflows) = project_workflows(project, package)? else {
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
    let sources = configuration_sources(project, package)?;

    let package_argument = if package == "cuenv" {
        String::new()
    } else {
        format!(" --package {}", shell_quote(package))
    };
    let command_path = format!("{path}{package_argument}");
    let plan_command = plan_command(&path, &command_path, &project.config.name);
    let (keep_plan, comment_job_steps) = if workflows.plan.comment {
        (
            vec![keep_plan_step()],
            vec![fetch_plan_step(), plan_comment_step(&project.config.name)],
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let comment_job = workflows.plan.comment.then(|| FollowUpJob {
        id_suffix: "comment",
        name: "Comment infrastructure plan",
        // `!cancelled()`: a plan cancelled by a newer push must not report
        // its partial output; a failed plan is still reported.
        if_condition: "!cancelled() && !contains(needs.*.result, 'skipped')",
        permissions: Permissions {
            pull_requests: Some(PermissionLevel::Write),
            ..Permissions::default()
        },
        steps: &comment_job_steps,
    });
    let plan = generate_workflow(&WorkflowRequest {
        project,
        package,
        project_path: project_path.as_deref(),
        sources: &sources,
        pipeline_name: PLAN_PIPELINE,
        pipeline: plan_pipeline(),
        command: &CommandWorkflow {
            step_name: "Plan infrastructure",
            step_id: Some(PLAN_STEP_ID),
            command: &plan_command,
            concurrency_group: None,
            cancel_in_progress: true,
            // The plan runs configuration the pull request controls (a
            // provider `path`, secret resolvers) with the plan's secrets:
            // only pull requests from this repository plan, and the job's
            // token can only read. Commenting is a separate job.
            job_if: Some(SAME_REPOSITORY),
            follow_up_steps: &keep_plan,
            job_environment: workflows.plan.environment.as_deref(),
            token_scope: TokenScope::Tasks,
            follow_up_job: comment_job.as_ref(),
            leading_steps: &[],
            additional_permissions: &[],
        },
    })?;

    let require_reviewed_plan = reviewed_plan_requirement(project, &workflows)?;
    let reviewed_steps = if require_reviewed_plan {
        vec![reviewed_plan_step(&project.config.name)]
    } else {
        Vec::new()
    };
    let expect_plan = if require_reviewed_plan {
        " --expect-plan \"$REVIEWED_PLAN\""
    } else {
        ""
    };
    let apply_command = format!(
        "cuenv infra init --path {path} &&\ncuenv infrastructure apply --yes{expect_plan} --path {command_path}"
    );
    // Pushes and manual runs apply only the configured branch.
    let apply_if = (workflows.apply.on == InfrastructureApplyTrigger::Push).then(|| {
        // GitHub expression strings escape a quote by doubling it.
        let branch = workflows.apply.branch.replace('\'', "''");
        format!("github.ref == 'refs/heads/{branch}'")
    });
    let apply = generate_workflow(&WorkflowRequest {
        project,
        package,
        project_path: project_path.as_deref(),
        sources: &sources,
        pipeline_name: APPLY_PIPELINE,
        pipeline: apply_pipeline(&workflows.apply),
        command: &CommandWorkflow {
            step_name: "Apply infrastructure",
            step_id: None,
            command: &apply_command,
            // One group for every ref, never cancelled: a cancelled apply can
            // leave the state lock held and resources half converged. GitHub
            // keeps one pending run per group and a newer one replaces it;
            // that is safe because every apply plans afresh from its own,
            // newer commit, which includes the replaced run's changes.
            concurrency_group: Some("${{ github.workflow }}"),
            cancel_in_progress: false,
            job_if: apply_if.as_deref(),
            follow_up_steps: &[],
            job_environment: workflows.apply.environment.as_deref(),
            token_scope: TokenScope::Workflow,
            follow_up_job: None,
            leading_steps: &reviewed_steps,
            additional_permissions: if require_reviewed_plan {
                &[("pull-requests", "read")]
            } else {
                &[]
            },
        },
    })?;

    Ok(vec![plan, apply])
}

/// The module-relative files the project's configuration is built from,
/// as the CUE loader resolves them: the project's own files, same-package
/// files of ancestor directories, and every in-module package it imports.
fn configuration_sources(project: &ProjectInfo, package: &str) -> Result<Vec<String>> {
    let mut sources = cuengine::instance_sources(
        &project.module_root,
        &project.project_path,
        package,
    )
    .map(|sources| sources.files)
    .map_err(|error| {
        cuenv_core::Error::configuration(format!(
            "Failed to list the CUE files of project '{}' for its infrastructure workflows: \
                 {error}",
            project.config.name
        ))
    })?;
    if let Some(locator) = load_locator(project, package)? {
        sources.extend(
            cuengine::instance_sources(&project.module_root, &locator.directory, &locator.package)
                .map_err(|error| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to list infrastructure CUE files for project '{}': {error}",
                        project.config.name
                    ))
                })?
                .files,
        );
        let directory = locator
            .directory
            .strip_prefix(&project.module_root)
            .map_err(|_| {
                cuenv_core::Error::configuration(
                    "Infrastructure directory is outside the project's CUE module",
                )
            })?;
        sources.push(if directory.as_os_str().is_empty() {
            "**".to_string()
        } else {
            format!("{}/**", directory.to_string_lossy().replace('\\', "/"))
        });
        // Explicit-file evaluation does not yet expose the locator's transitive
        // imports. Cover every CUE file, including dotfiles, rather than omit a
        // dependency from the workflow triggers.
        sources.push("**/*.cue".to_string());
        sources.push("**/.infrastructure.cue".to_string());
    }
    // The module's lockfile pins provider releases and schema digests, and
    // `cuenv infra init` restores generated types from it: a lock-only change
    // changes what plan and apply run, wherever the project lives.
    sources.push("cuenv.lock".to_string());
    sources.sort();
    sources.dedup();
    Ok(sources)
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
    let manual = (apply.manual || apply.on == InfrastructureApplyTrigger::Manual).then(|| {
        if apply.require_reviewed_plan {
            ManualTrigger::WithInputs(std::collections::HashMap::from([(
                PLAN_DIGEST_INPUT.to_string(),
                cuenv_core::ci::WorkflowDispatchInput {
                    description: "Reviewed plan digest (sha256:…) of the plan to apply, as \
                                  `cuenv infrastructure plan` printed it"
                        .to_string(),
                    required: Some(true),
                    default: None,
                    input_type: Some("string".to_string()),
                    options: None,
                },
            )]))
        } else {
            ManualTrigger::Enabled(true)
        }
    });
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
fn plan_command(path: &str, command_path: &str, project_name: &str) -> String {
    format!(
        "set -o pipefail\n\
         cuenv infra init --path {path} || exit $?\n\
         status=0\n\
         cuenv infrastructure plan --path {command_path} 2>&1 | tee \"{PLAN_FILE}\" || status=$?\n\
         {{\n  \
           printf '### Infrastructure plan: `{project_name}`\\n\\n````diff\\n'\n  \
           cat \"{PLAN_FILE}\"\n  \
           printf '````\\n'\n\
         }} >> \"$GITHUB_STEP_SUMMARY\"\n\
         exit \"$status\""
    )
}

/// Keep the plan output for the comment job, which has no secrets.
fn keep_plan_step() -> Step {
    Step::uses("actions/upload-artifact@v4")
        .with_name("Keep the plan for the comment")
        .with_if(format!(
            "!cancelled() && steps.{PLAN_STEP_ID}.outcome != 'skipped'"
        ))
        .with_input("name", serde_yaml::Value::String(PLAN_ARTIFACT.to_string()))
        .with_input("path", serde_yaml::Value::String(PLAN_FILE.to_string()))
        .with_input("retention-days", serde_yaml::Value::Number(1.into()))
        .with_input(
            "if-no-files-found",
            serde_yaml::Value::String("ignore".to_string()),
        )
}

/// Fetch the plan output in the comment job.
fn fetch_plan_step() -> Step {
    let mut step = Step::uses("actions/download-artifact@v4")
        .with_name("Fetch the plan")
        .with_input("name", serde_yaml::Value::String(PLAN_ARTIFACT.to_string()))
        .with_input(
            "path",
            serde_yaml::Value::String("${{ runner.temp }}".to_string()),
        );
    // A plan that failed before writing output still gets its comment.
    step.continue_on_error = Some(true);
    step
}

/// Create or edit the pull request comment that shows the plan. A hidden
/// marker per project finds the comment an earlier run posted; only comments
/// by the workflow's own token count, so nobody else's comment is edited.
/// A second marker records the pull request head the plan was made for, so
/// an apply that requires the reviewed plan can tell a stale comment.
fn plan_comment_step(project_name: &str) -> Step {
    let script = format!(
        r"const fs = require('fs');
const marker = '<!-- cuenv-infrastructure-plan:{project_name} -->';
const head = context.payload.pull_request.head.sha;
const file = process.env.PLAN_FILE;
let plan = fs.existsSync(file) ? fs.readFileSync(file, 'utf8').trimEnd() : '';
if (plan === '') plan = 'The plan produced no output.';
const limit = 60000;
if (plan.length > limit) plan = `${{plan.slice(0, limit)}}\n… (truncated; the job log has the full plan)`;
const failed = process.env.PLAN_OUTCOME !== 'success';
const run = `${{context.serverUrl}}/${{context.repo.owner}}/${{context.repo.repo}}/actions/runs/${{context.runId}}`;
const body = [
  marker,
  `<!-- cuenv-infrastructure-plan-head:${{head}} -->`,
  `### ${{failed ? '❌ Infrastructure plan failed' : 'Infrastructure plan'}}: \`{project_name}\``,
  '',
  '````diff',
  plan,
  '````',
  '',
  `[Workflow run](${{run}}) · ${{head.slice(0, 7)}}`,
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
    Step::uses("actions/github-script@v7")
        .with_name("Comment infrastructure plan")
        .with_env("PLAN_FILE", PLAN_FILE)
        .with_env("PLAN_OUTCOME", "${{ join(needs.*.result, '') }}")
        .with_input("script", serde_yaml::Value::String(script))
}

/// Whether the apply must require the reviewed plan, refusing settings that
/// cannot name one.
fn reviewed_plan_requirement(
    project: &ProjectInfo,
    workflows: &InfrastructureWorkflows,
) -> Result<bool> {
    let apply = &workflows.apply;
    if !apply.require_reviewed_plan {
        return Ok(false);
    }
    let refuse = |reason: &str| {
        Err(cuenv_core::Error::configuration(format!(
            "Project '{}' sets infrastructure.workflows.apply.requireReviewedPlan, {reason}",
            project.config.name
        )))
    };
    match apply.on {
        InfrastructureApplyTrigger::Tag | InfrastructureApplyTrigger::Release => refuse(
            "but a tag or release has no pull request to take the reviewed plan from; use \
             `on: \"push\"` or `\"manual\"`",
        ),
        InfrastructureApplyTrigger::Push if !workflows.plan.comment => refuse(
            "but the reviewed plan is read from the plan comment; keep `plan.comment` enabled",
        ),
        InfrastructureApplyTrigger::Push | InfrastructureApplyTrigger::Manual => Ok(true),
    }
}

/// Find the reviewed plan digest: a manual run's `plan_digest` input, or the
/// digest in the plan comment of the pull request this push merged, made for
/// that pull request's final head. Exports it as `REVIEWED_PLAN`; fails when
/// there is none.
fn reviewed_plan_step(project_name: &str) -> Step {
    let script = format!(
        r"const marker = '<!-- cuenv-infrastructure-plan:{project_name} -->';
const pattern = /Reviewed plan digest: (sha256:[0-9a-f]{{64}})/;
const manual = context.payload.inputs?.{PLAN_DIGEST_INPUT};
if (context.eventName === 'workflow_dispatch') {{
  if (!/^sha256:[0-9a-f]{{64}}$/.test(manual ?? '')) {{
    core.setFailed('Pass the reviewed plan digest (sha256:…) as the {PLAN_DIGEST_INPUT} input.');
    return;
  }}
  core.exportVariable('REVIEWED_PLAN', manual);
  return;
}}
const {{ owner, repo }} = context.repo;
const pulls = await github.rest.repos.listPullRequestsAssociatedWithCommit({{ owner, repo, commit_sha: context.sha }});
const pull = pulls.data.find((candidate) => candidate.merged_at && candidate.merge_commit_sha === context.sha);
if (!pull) {{
  core.setFailed(`No merged pull request produced ${{context.sha}}, so there is no reviewed plan to apply. Merge through a pull request, or run this workflow manually with the reviewed plan digest.`);
  return;
}}
const comments = await github.paginate(github.rest.issues.listComments, {{ owner, repo, issue_number: pull.number, per_page: 100 }});
const comment = comments.find((candidate) =>
  candidate.user?.login === 'github-actions[bot]' && candidate.body?.includes(marker));
if (!comment?.body?.includes(`<!-- cuenv-infrastructure-plan-head:${{pull.head.sha}} -->`)) {{
  core.setFailed(`Pull request #${{pull.number}} has no plan comment for its final commit ${{pull.head.sha}}; the reviewed plan is unknown.`);
  return;
}}
const match = comment.body.match(pattern);
if (!match) {{
  core.setFailed(`The plan comment of pull request #${{pull.number}} names no reviewed plan digest.`);
  return;
}}
core.exportVariable('REVIEWED_PLAN', match[1]);"
    );
    Step::uses("actions/github-script@v7")
        .with_name("Find the reviewed plan")
        .with_id(REVIEWED_PLAN_STEP_ID)
        .with_input("script", serde_yaml::Value::String(script))
}

struct WorkflowRequest<'request> {
    project: &'request ProjectInfo,
    package: &'request str,
    project_path: Option<&'request str>,
    /// Configuration files whose changes trigger the workflow, in addition
    /// to the paths the compiler derives.
    sources: &'request [String],
    pipeline_name: &'request str,
    pipeline: Pipeline,
    command: &'request CommandWorkflow<'request>,
}

fn generate_workflow(request: &WorkflowRequest<'_>) -> Result<(String, String)> {
    use cuenv_ci::compiler::{Compiler, CompilerOptions};

    let WorkflowRequest {
        project,
        package,
        project_path,
        sources,
        pipeline_name,
        ref pipeline,
        command,
    } = *request;

    // The compiler derives triggers only for projects with a `ci` block.
    let mut config = project.config.clone();
    // Contributor discovery must also see secret references used only by the
    // separate infrastructure package (for example its state credential).
    config.infrastructure = infrastructure_configuration(project, package)?;
    let github_config = config
        .ci
        .get_or_insert_with(CI::default)
        .github_config_for_pipeline(pipeline_name);

    let options = CompilerOptions {
        pipeline_name: Some(pipeline_name.to_string()),
        pipeline: Some(pipeline.clone()),
        ci_mode: true,
        module_root: Some(project.module_root.clone()),
        file_anchors: project.file_anchors.as_ref().clone(),
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
    // A pipeline without tasks derives only the project directory and
    // `cue.mod/**`; add every file the configuration is loaded from, so a
    // change to an imported package elsewhere in the module triggers too.
    if let Some(trigger) = ir.pipeline.trigger.as_mut()
        && !trigger.paths.is_empty()
    {
        let directories: Vec<String> = trigger
            .paths
            .iter()
            .filter_map(|path| path.strip_suffix("**"))
            .map(str::to_string)
            .collect();
        trigger.paths.extend(
            sources
                .iter()
                .filter(|source| {
                    !directories
                        .iter()
                        .any(|directory| source.starts_with(directory))
                })
                .cloned(),
        );
        trigger.paths.sort();
        trigger.paths.dedup();
    }

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

    /// A project in a CUE module on disk: `infrastructure/dns` imports the
    /// module's `shared` package and its own `zones` subpackage.
    struct Fixture {
        _module: tempfile::TempDir,
        info: ProjectInfo,
    }

    fn project(workflows: &serde_json::Value) -> Fixture {
        let module = tempfile::tempdir().expect("temporary module");
        for (file, content) in [
            (
                "cue.mod/module.cue",
                "module: \"example.com/infra@v0\"\nlanguage: version: \"v0.14.1\"\n",
            ),
            (
                "infrastructure/dns/env.cue",
                "package cuenv\n\nimport (\n\t\"example.com/infra/shared\"\n\t\"example.com/infra/infrastructure/dns/zones\"\n)\n\nname: shared.name\nzone: zones.name\n",
            ),
            (
                "infrastructure/dns/zones/zones.cue",
                "package zones\n\nname: \"example.com\"\n",
            ),
            ("shared/shared.cue", "package shared\n\nname: \"dns\"\n"),
        ] {
            let path = module.path().join(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
            std::fs::write(path, content).expect("file");
        }
        let mut config = Project::new("dns");
        config.infrastructure = Some(serde_json::json!({
            "state": {"turso": {"url": "http://127.0.0.1:8080"}},
            "workflows": workflows,
        }));
        let info = ProjectInfo {
            project_path: module.path().join("infrastructure/dns"),
            relative_path: PathBuf::from("infrastructure/dns"),
            module_root: module.path().to_path_buf(),
            file_anchors: std::sync::Arc::default(),
            config,
        };
        Fixture {
            _module: module,
            info,
        }
    }

    fn workflows(
        workflows: &serde_json::Value,
    ) -> std::result::Result<Vec<serde_yaml::Value>, Box<dyn std::error::Error>> {
        let generated = generate_workflows(&project(workflows).info, "cuenv")?;
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
        let mut fixture = project(&serde_json::Value::Null);
        fixture.info.config.infrastructure = Some(serde_json::json!({"state": {}}));
        assert!(generate_workflows(&fixture.info, "cuenv")?.is_empty());
        fixture.info.config.infrastructure = None;
        assert!(generate_workflows(&fixture.info, "cuenv")?.is_empty());
        Ok(())
    }

    #[test]
    fn invalid_workflows_are_a_configuration_error() {
        let fixture = project(&serde_json::json!({"apply": {"on": "nightly"}}));
        let error = generate_workflows(&fixture.info, "cuenv").expect_err("invalid apply.on");
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

        let plan_paths: Vec<&str> = plan["on"]["pull_request"]["paths"]
            .as_sequence()
            .ok_or("plan paths")?
            .iter()
            .filter_map(serde_yaml::Value::as_str)
            .collect();
        for path in [
            ".github/workflows/dns-infrastructure-plan.yml",
            "cue.mod/**",
            "infrastructure/dns/**",
            // The directories the project's CUE is built from, so a file
            // added to one of them starts the workflow too.
            "infrastructure/dns/*.cue",
            "shared/*.cue",
            "shared/shared.cue",
            // A lock-only change re-plans and applies an inline project too.
            "cuenv.lock",
        ] {
            assert!(plan_paths.contains(&path), "{path} in {plan_paths:?}");
        }
        assert_eq!(
            apply["on"]["push"]["paths"].as_sequence().map(Vec::len),
            Some(plan_paths.len())
        );
        // Files under a derived directory glob are not listed again.
        assert!(
            !plan_paths.contains(&"infrastructure/dns/zones/zones.cue"),
            "{plan_paths:?}"
        );
        assert!(plan["on"].get("push").is_none());
        assert_eq!(plan["concurrency"]["cancel-in-progress"], true);
        // The plan job runs code the pull request controls: its token only
        // reads, and only pull requests from this repository plan.
        let job = &plan["jobs"]["dns-infrastructure-plan"];
        assert_eq!(job["permissions"]["contents"], "read");
        assert!(job["permissions"].get("pull-requests").is_none());
        assert!(job["permissions"].get("checks").is_none());
        assert_eq!(job["if"], SAME_REPOSITORY);
        let plan_steps = steps(plan, "dns-infrastructure-plan");
        let plan_step = step(&plan_steps, "Plan infrastructure").ok_or("plan step")?;
        assert_eq!(plan_step["id"], "plan");
        let run = plan_step["run"].as_str().ok_or("plan run")?;
        assert!(run.starts_with("set -o pipefail\n"), "{run}");
        assert!(
            run.contains("cuenv infra init --path infrastructure/dns || exit $?\n"),
            "{run}"
        );
        assert!(
            run.contains("cuenv infrastructure plan --path infrastructure/dns 2>&1 | tee"),
            "{run}"
        );
        assert!(run.contains("$GITHUB_STEP_SUMMARY"), "{run}");
        assert!(step(&plan_steps, "Comment infrastructure plan").is_none());
        let keep = step(&plan_steps, "Keep the plan for the comment").ok_or("keep step")?;
        assert_eq!(keep["uses"], "actions/upload-artifact@v4");

        // A separate job without the plan's secrets posts the comment.
        let comment_job = &plan["jobs"]["dns-infrastructure-plan-comment"];
        assert_eq!(comment_job["needs"][0], "dns-infrastructure-plan");
        assert_eq!(comment_job["permissions"]["pull-requests"], "write");
        assert!(comment_job["permissions"].get("contents").is_none());
        assert!(comment_job.get("environment").is_none());
        let comment_steps = steps(plan, "dns-infrastructure-plan-comment");
        assert_eq!(comment_steps.len(), 2);
        assert_eq!(comment_steps[0]["uses"], "actions/download-artifact@v4");
        let comment = step(&comment_steps, "Comment infrastructure plan").ok_or("comment step")?;
        assert_eq!(comment["uses"], "actions/github-script@v7");
        assert_eq!(
            comment["env"]["PLAN_OUTCOME"],
            "${{ join(needs.*.result, '') }}"
        );
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
            "cuenv infra init --path infrastructure/dns &&\ncuenv infrastructure apply --yes --path infrastructure/dns"
        );
        assert!(step(&apply_steps, "Comment infrastructure plan").is_none());
        assert!(step(&apply_steps, "Find the reviewed plan").is_none());
        Ok(())
    }

    #[test]
    fn the_plan_job_can_take_its_secrets_from_a_github_environment()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({"plan": {"environment": "plan-readonly"}}))?;
        let plan = &generated[0];
        assert_eq!(
            plan["jobs"]["dns-infrastructure-plan"]["environment"],
            "plan-readonly"
        );
        assert!(
            plan["jobs"]["dns-infrastructure-plan-comment"]
                .get("environment")
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn an_apply_can_require_the_reviewed_plan()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({"apply": {"requireReviewedPlan": true}}))?;
        let apply = &generated[1];
        let job = &apply["jobs"]["dns-infrastructure-apply"];
        assert_eq!(job["permissions"]["pull-requests"], "read");
        let apply_steps = steps(apply, "dns-infrastructure-apply");
        let names: Vec<&str> = apply_steps
            .iter()
            .filter_map(|step| step["name"].as_str())
            .collect();
        let find = names
            .iter()
            .position(|name| *name == "Find the reviewed plan")
            .ok_or("find step")?;
        let run = names
            .iter()
            .position(|name| *name == "Apply infrastructure")
            .ok_or("apply step")?;
        assert!(find < run, "{names:?}");
        let script = apply_steps[find]["with"]["script"]
            .as_str()
            .ok_or("script")?;
        for needle in [
            "listPullRequestsAssociatedWithCommit",
            "<!-- cuenv-infrastructure-plan:dns -->",
            "cuenv-infrastructure-plan-head:${pull.head.sha}",
            "core.exportVariable('REVIEWED_PLAN'",
            "context.payload.inputs?.plan_digest",
        ] {
            assert!(script.contains(needle), "{needle} in {script}");
        }
        assert_eq!(
            apply_steps[run]["run"],
            "cuenv infra init --path infrastructure/dns &&\ncuenv infrastructure apply --yes --expect-plan \"$REVIEWED_PLAN\" --path infrastructure/dns"
        );
        let input = &apply["on"]["workflow_dispatch"]["inputs"]["plan_digest"];
        assert_eq!(input["required"], true);

        // The comment the apply reads names the head it was made for.
        let comment_steps = steps(&generated[0], "dns-infrastructure-plan-comment");
        let comment = step(&comment_steps, "Comment infrastructure plan").ok_or("comment")?;
        let script = comment["with"]["script"].as_str().ok_or("script")?;
        assert!(
            script.contains("cuenv-infrastructure-plan-head:${head}"),
            "{script}"
        );
        Ok(())
    }

    #[test]
    fn a_reviewed_plan_needs_a_pull_request_to_come_from() {
        for workflows in [
            serde_json::json!({"apply": {"on": "tag", "requireReviewedPlan": true}}),
            serde_json::json!({"apply": {"on": "release", "requireReviewedPlan": true}}),
            serde_json::json!({"plan": {"comment": false}, "apply": {"requireReviewedPlan": true}}),
        ] {
            let error = generate_workflows(&project(&workflows).info, "cuenv")
                .unwrap_err()
                .to_string();
            assert!(error.contains("requireReviewedPlan"), "{error}");
        }
        assert!(
            generate_workflows(
                &project(
                    &serde_json::json!({"apply": {"on": "manual", "requireReviewedPlan": true}})
                )
                .info,
                "cuenv"
            )
            .is_ok()
        );
    }

    #[test]
    fn plan_comment_can_be_disabled() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({"plan": {"comment": false}}))?;
        let plan_steps = steps(&generated[0], "dns-infrastructure-plan");
        assert!(step(&plan_steps, "Plan infrastructure").is_some());
        assert!(step(&plan_steps, "Keep the plan for the comment").is_none());
        assert!(
            generated[0]["jobs"]
                .get("dns-infrastructure-plan-comment")
                .is_none()
        );
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
        assert_eq!(
            plan["jobs"]["dns-infrastructure-plan"]["if"],
            SAME_REPOSITORY
        );
        Ok(())
    }

    #[test]
    fn comment_skips_cancelled_runs_and_foreign_comments()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let generated = workflows(&serde_json::json!({}))?;
        let comment_job = &generated[0]["jobs"]["dns-infrastructure-plan-comment"];
        let condition = comment_job["if"].as_str().ok_or("condition")?;
        assert!(condition.starts_with("!cancelled() && "), "{condition}");
        // A fork's pull request skips the plan job, and so the comment.
        assert!(condition.contains("'skipped'"), "{condition}");
        let comment_steps = steps(&generated[0], "dns-infrastructure-plan-comment");
        let comment = step(&comment_steps, "Comment infrastructure plan").ok_or("comment step")?;
        let script = comment["with"]["script"].as_str().ok_or("script")?;
        assert!(
            script.contains("comment.user?.login === 'github-actions[bot]'"),
            "{script}"
        );
        Ok(())
    }

    #[test]
    fn separate_infrastructure_projects_generate_workflows_and_cover_sources()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut fixture = project(&serde_json::json!({}));
        fixture.info.config.infrastructure = None;
        std::fs::write(
            fixture.info.project_path.join(".infrastructure.cue"),
            r#"package configuration
directory: "../../stacks/dns"
"package": "stack"
"#,
        )?;
        let directory = fixture.info.module_root.join("stacks/dns");
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("main.cue"),
            r#"package stack
import "example.com/infra/shared"
infrastructure: {
    workflows: {}
    state: d1: token: {resolver: "onepassword", ref: "op://vault/state/token"}
    resources: pending: configuration: name: string
    label: shared.name
}
"#,
        )?;
        fixture.info.config.ci = Some(serde_json::from_value(serde_json::json!({
            "contributors": [{
                "id": "secret-bootstrap",
                "when": {"secretsProvider": ["onepassword"]},
                "tasks": [{"id": "setup", "command": "echo", "args": ["prepare-state-secrets"], "priority": 10}]
            }]
        }))?);
        let generated = generate_workflows(&fixture.info, "cuenv")?;
        assert_eq!(generated.len(), 2);
        for (_, yaml) in &generated {
            assert!(yaml.contains("prepare-state-secrets"), "{yaml}");
        }
        let plan: serde_yaml::Value = serde_yaml::from_str(&generated[0].1)?;
        let paths = plan["on"]["pull_request"]["paths"]
            .as_sequence()
            .ok_or("paths")?;
        for source in [
            "stacks/dns/**",
            "**/*.cue",
            "**/.infrastructure.cue",
            "cuenv.lock",
            "shared/shared.cue",
        ] {
            assert!(
                paths.iter().any(|path| path.as_str() == Some(source)),
                "{source}: {paths:?}"
            );
        }
        assert!(stale_workflows(&fixture.info, "cuenv")?.is_empty());
        std::fs::write(
            directory.join("main.cue"),
            "package stack\ninfrastructure: resources: pending: configuration: name: string\n",
        )?;
        assert!(generate_workflows(&fixture.info, "cuenv")?.is_empty());
        assert_eq!(stale_workflows(&fixture.info, "cuenv")?.len(), 2);
        fixture.info.config.infrastructure = Some(serde_json::json!({"workflows": {}}));
        let ambiguous =
            generate_workflows(&fixture.info, "cuenv").expect_err("ambiguous infrastructure");
        assert!(ambiguous.to_string().contains("both inline infrastructure"));
        fixture.info.config.infrastructure = None;
        std::fs::write(
            directory.join("main.cue"),
            "package another\ninfrastructure: workflows: {}\n",
        )?;
        let missing = generate_workflows(&fixture.info, "cuenv").expect_err("missing package");
        assert!(missing.to_string().contains("was not found"));
        Ok(())
    }

    #[test]
    fn locator_workflows_belong_to_the_explicit_project_package()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut fixture = project(&serde_json::json!({}));
        fixture.info.config.infrastructure = None;
        let directory = fixture.info.project_path.join("stack");
        std::fs::create_dir(&directory)?;
        std::fs::write(
            directory.join("main.cue"),
            "package stack\ninfrastructure: workflows: {}\n",
        )?;
        std::fs::write(
            fixture.info.project_path.join("env.cue"),
            "package operations\nname: \"dns\"\n",
        )?;
        std::fs::write(
            fixture.info.project_path.join(".infrastructure.cue"),
            r#"package configuration
directory: "stack"
"package": "stack"
projectPackage: "operations"
"#,
        )?;
        assert!(generate_workflows(&fixture.info, "cuenv")?.is_empty());
        assert!(stale_workflows(&fixture.info, "cuenv")?.is_empty());
        let generated = generate_workflows(&fixture.info, "operations")?;
        assert_eq!(generated.len(), 2);
        for (_, yaml) in generated {
            assert!(yaml.contains("--package operations"), "{yaml}");
            assert!(
                !yaml.contains("init --path infrastructure/dns --package"),
                "{yaml}"
            );
        }
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
        let mut fixture = project(&serde_json::json!({}));
        let mut ci = CI::default();
        ci.pipelines
            .insert(APPLY_PIPELINE.to_string(), Pipeline::default());
        fixture.info.config.ci = Some(ci);
        let error = generate_workflows(&fixture.info, "cuenv").expect_err("name collision");
        assert!(
            error.to_string().contains("infrastructure-apply"),
            "{error}"
        );
    }

    #[test]
    fn disabling_workflows_marks_their_files_stale()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut fixture = project(&serde_json::json!({}));
        let info = &mut fixture.info;
        info.config.name = "rawkode.academy".to_string();
        assert!(stale_workflows(info, "cuenv")?.is_empty());
        info.config.infrastructure = Some(serde_json::json!({"state": {}}));
        assert_eq!(
            stale_workflows(info, "cuenv")?,
            [
                "rawkodeacademy-infrastructure-plan.yml",
                "rawkodeacademy-infrastructure-apply.yml"
            ]
        );
        info.config.infrastructure = None;
        assert!(stale_workflows(info, "cuenv")?.is_empty());
        Ok(())
    }
}
