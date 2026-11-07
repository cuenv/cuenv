//! GitHub workflows for `infrastructure.workflows`.
//!
//! A project with `github` in `ci.providers` that sets
//! `infrastructure.workflows` gets two workflows:
//! one that diffs the configuration on pull requests (and comments the diff)
//! and one that applies on `apply.on`. Both use the thin workflow layout, so
//! the project's `ci` contributors install cuenv before the main step runs.
//! The diff evaluates CUE only, so its job leaves out every contributor that
//! sets up a secret provider or asks for credentials; the apply job keeps
//! them.

use std::collections::HashSet;

use super::ProjectInfo;
use cuenv_core::Result;
use cuenv_core::ci::{CI, ManualTrigger, Pipeline, PipelineCondition, StringOrVec};
use cuenv_core::manifest::{
    InfrastructureApplyTrigger, InfrastructureApplyWorkflow, InfrastructureWorkflows, Project,
};
use cuenv_github::GitHubConfigExt;
use cuenv_github::workflow::schema::Step;
use cuenv_github::workflow::{CheckoutHistory, CommandWorkflow, GitHubActionsEmitter};

/// Pipeline names of the diff and apply workflows; the workflow files are
/// `<project>-<pipeline>.yml`.
const DIFF_PIPELINE: &str = "infrastructure-diff";
const APPLY_PIPELINE: &str = "infrastructure-apply";

/// The pull request workflow earlier versions wrote, which ran `plan` with
/// credentials; sync removes its file.
const RETIRED_PLAN_PIPELINE: &str = "infrastructure-plan";

/// Where the diff step writes the diff (Markdown) for the comment step.
const DIFF_FILE: &str = "${{ runner.temp }}/cuenv-infrastructure-diff.md";

/// Where the diff step writes the errors of a failed diff.
const DIFF_ERRORS_FILE: &str = "${{ runner.temp }}/cuenv-infrastructure-diff-errors.txt";

/// The id of the diff step, read by the comment step.
const DIFF_STEP_ID: &str = "diff";

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
    if workflows.get("plan").is_some() {
        return Err(cuenv_core::Error::configuration(format!(
            "infrastructure.workflows.plan in project '{}' was renamed to \
             infrastructure.workflows.diff: pull requests now get a diff of the configuration \
             that needs no credentials, instead of a plan",
            project.name
        )));
    }
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

/// Workflow files of a project with infrastructure that an earlier sync may
/// have written and this one does not: the diff and apply workflows when
/// the project does not set `infrastructure.workflows`, and the retired plan
/// workflow either way. A file the project's own `ci.pipelines` writes is
/// never stale.
pub(super) fn stale_workflows(project: &ProjectInfo, package: &str) -> Result<Vec<String>> {
    if project.config.infrastructure.is_none() && load_locator(project, package)?.is_none() {
        return Ok(Vec::new());
    }
    let pipelines: &[&str] = if project_workflows(project, package)?.is_some() {
        &[RETIRED_PLAN_PIPELINE]
    } else {
        &[DIFF_PIPELINE, APPLY_PIPELINE, RETIRED_PLAN_PIPELINE]
    };
    Ok(pipelines
        .iter()
        .filter(|pipeline| !defines_pipeline(&project.config, pipeline))
        .map(|pipeline| workflow_filename(&project.config.name, pipeline))
        .collect())
}

/// Whether the project's `ci.pipelines` has a pipeline with this name, whose
/// workflow file is `<project>-<pipeline>.yml` too.
fn defines_pipeline(project: &Project, pipeline: &str) -> bool {
    project
        .ci
        .as_ref()
        .is_some_and(|ci| ci.pipelines.contains_key(pipeline))
}

fn workflow_filename(project_name: &str, pipeline_name: &str) -> String {
    format!(
        "{}.yml",
        super::sanitize_workflow_name(&format!("{project_name}-{pipeline_name}"))
    )
}

/// Generate the diff and apply workflows for a project, or nothing when the
/// project does not set `infrastructure.workflows`.
pub(super) fn generate_workflows(
    project: &ProjectInfo,
    package: &str,
) -> Result<Vec<(String, String)>> {
    let Some(workflows) = project_workflows(project, package)? else {
        return Ok(Vec::new());
    };
    for name in [DIFF_PIPELINE, APPLY_PIPELINE] {
        if defines_pipeline(&project.config, name) {
            return Err(cuenv_core::Error::configuration(format!(
                "Project '{}' defines a ci pipeline named '{name}', whose workflow file \
                 infrastructure.workflows also writes. Rename the pipeline.",
                project.config.name
            )));
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
    let diff_command = diff_command(&path, &command_path, &project.config.name);
    let comment_steps: Vec<Step> = if workflows.diff.comment {
        vec![diff_comment_step(&project.config.name)]
    } else {
        Vec::new()
    };
    let diff = generate_workflow(&WorkflowRequest {
        project,
        package,
        project_path: project_path.as_deref(),
        sources: &sources,
        pipeline_name: DIFF_PIPELINE,
        pipeline: diff_pipeline(),
        credentials: Credentials::Withheld,
        command: &CommandWorkflow {
            step_name: "Diff infrastructure",
            step_id: Some(DIFF_STEP_ID),
            command: &diff_command,
            concurrency_group: None,
            cancel_in_progress: true,
            job_if: None,
            follow_up_steps: &comment_steps,
            job_environment: None,
            // The base branch must be there to compare with.
            history: CheckoutHistory::Full,
        },
    })?;

    let apply_command = format!(
        "cuenv infra init --path {path} &&\ncuenv infrastructure apply --yes --path {command_path}"
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
        credentials: Credentials::SetUp,
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
            history: CheckoutHistory::Recent,
        },
    })?;

    Ok(vec![diff, apply])
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
        sources.push("cuenv.lock".to_string());
        sources.sort();
        sources.dedup();
    }
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

fn diff_pipeline() -> Pipeline {
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

/// Diff the configuration with the pull request's base branch, keep the
/// Markdown and the errors for the comment step, show both in the log and
/// add them to the job summary, then exit with the diff's status.
///
/// `infra init` restores the generated provider types the configuration
/// may import; it downloads public provider releases and needs no
/// credential. The base branch comes from `GITHUB_BASE_REF`, never from an
/// expression spliced into the script.
fn diff_command(path: &str, command_path: &str, project_name: &str) -> String {
    let heading = shell_quote(&format!("### Infrastructure changes: `{project_name}`"));
    format!(
        "cuenv infra init --path {path} || exit $?\n\
         status=0\n\
         cuenv infrastructure diff --base \"origin/$GITHUB_BASE_REF\" --path {command_path} \
         > \"{DIFF_FILE}\" 2> \"{DIFF_ERRORS_FILE}\" || status=$?\n\
         cat \"{DIFF_FILE}\"\n\
         cat \"{DIFF_ERRORS_FILE}\" >&2\n\
         {{\n  \
           printf '%s\\n\\n' {heading}\n  \
           cat \"{DIFF_FILE}\"\n  \
           if [ \"$status\" -ne 0 ]; then\n    \
             printf '\\n````text\\n'\n    \
             cat \"{DIFF_ERRORS_FILE}\"\n    \
             printf '````\\n'\n  \
           fi\n\
         }} >> \"$GITHUB_STEP_SUMMARY\"\n\
         exit \"$status\""
    )
}

/// The comment step's script. `__MARKER__`, `__RETIRED_MARKER__` and
/// `__PROJECT__` are replaced by JavaScript string literals.
const DIFF_COMMENT_SCRIPT: &str = r"const fs = require('fs');
const marker = __MARKER__;
const retiredMarker = __RETIRED_MARKER__;
const project = __PROJECT__;
const read = (file) => (file && fs.existsSync(file) ? fs.readFileSync(file, 'utf8').trimEnd() : '');
const limit = 60000;
const truncate = (text) =>
  text.length > limit ? `${text.slice(0, limit)}\n\n… (truncated; the job summary has the whole output)` : text;
const failed = process.env.DIFF_OUTCOME !== 'success';
const diff = truncate(read(process.env.DIFF_FILE));
const run = `${context.serverUrl}/${context.repo.owner}/${context.repo.repo}/actions/runs/${context.runId}`;
const lines = [
  marker,
  `### ${failed ? '❌ Infrastructure diff failed' : 'Infrastructure changes'}: \`${project}\``,
  '',
];
if (diff !== '') {
  lines.push(diff, '');
} else if (!failed) {
  lines.push('The diff produced no output.', '');
}
if (failed) {
  const errors = truncate(read(process.env.DIFF_ERRORS_FILE)) || 'The diff reported no error; see the job log.';
  lines.push('````text', errors, '````', '');
}
lines.push(`[Workflow run](${run}) · ${context.sha.slice(0, 7)}`);
const body = lines.join('\n');
const { owner, repo } = context.repo;
const issue_number = context.issue.number;
const comments = await github.paginate(github.rest.issues.listComments, { owner, repo, issue_number, per_page: 100 });
const ours = (comment) => comment.user?.login === 'github-actions[bot]';
// The comment an earlier plan workflow posted becomes this one.
const existing =
  comments.find((comment) => ours(comment) && comment.body?.includes(marker)) ??
  comments.find((comment) => ours(comment) && comment.body?.includes(retiredMarker));
if (existing) {
  await github.rest.issues.updateComment({ owner, repo, comment_id: existing.id, body });
} else {
  await github.rest.issues.createComment({ owner, repo, issue_number, body });
}";

/// A JavaScript string literal (JSON is a subset of JavaScript).
fn javascript_string(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

/// Create or edit the pull request comment that shows the diff. A hidden
/// marker per project finds the comment an earlier run posted (or the plan
/// comment of the retired plan workflow); only comments by the workflow's own
/// token count, so nobody else's comment is edited.
fn diff_comment_step(project_name: &str) -> Step {
    let script = DIFF_COMMENT_SCRIPT
        .replace(
            "__RETIRED_MARKER__",
            &javascript_string(&format!(
                "<!-- cuenv-infrastructure-plan:{project_name} -->"
            )),
        )
        .replace(
            "__MARKER__",
            &javascript_string(&format!(
                "<!-- cuenv-infrastructure-diff:{project_name} -->"
            )),
        )
        .replace("__PROJECT__", &javascript_string(project_name));
    // `!cancelled()`: a diff cancelled by a newer push must not report its
    // partial output as a failure.
    Step::uses("actions/github-script@v7")
        .with_name("Comment infrastructure changes")
        .with_if(format!(
            "!cancelled() && steps.{DIFF_STEP_ID}.outcome != 'skipped' && \
             github.event.pull_request.head.repo.full_name == github.repository"
        ))
        .with_env("DIFF_FILE", DIFF_FILE)
        .with_env("DIFF_ERRORS_FILE", DIFF_ERRORS_FILE)
        .with_env(
            "DIFF_OUTCOME",
            format!("${{{{ steps.{DIFF_STEP_ID}.outcome }}}}"),
        )
        .with_input("script", serde_yaml::Value::String(script))
}

/// Whether a workflow's job sets up the secret providers the project uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Credentials {
    /// Run every contributor, so the job can resolve the project's secrets.
    SetUp,
    /// Leave out every contributor that sets up a secret provider or asks for
    /// credentials: the job runs with none.
    Withheld,
}

/// The contributors that set up a secret provider: those that a
/// `secretsProvider` condition activates.
fn secret_provider_contributors(project: &Project) -> HashSet<String> {
    project
        .ci
        .iter()
        .flat_map(|ci| &ci.contributors)
        .filter(|contributor| {
            contributor
                .when
                .as_ref()
                .is_some_and(|when| !when.secrets_provider.is_empty())
        })
        .map(|contributor| contributor.id.clone())
        .collect()
}

/// Whether a contributor's task gives the job credentials: it belongs to a
/// secret provider's contributor, reads secrets, or asks GitHub for an OIDC
/// token (`id-token` permission).
fn grants_credentials(task: &cuenv_ci::ir::Task, secret_providers: &HashSet<String>) -> bool {
    task.contributor
        .as_ref()
        .is_some_and(|contributor| secret_providers.contains(contributor))
        || !task.secrets.is_empty()
        || task.github_permissions.contains_key("id-token")
}

struct WorkflowRequest<'a> {
    project: &'a ProjectInfo,
    package: &'a str,
    project_path: Option<&'a str>,
    /// Configuration files whose changes trigger the workflow, in addition
    /// to the paths the compiler derives.
    sources: &'a [String],
    pipeline_name: &'a str,
    pipeline: Pipeline,
    credentials: Credentials,
    command: &'a CommandWorkflow<'a>,
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
        credentials,
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
    let secret_providers = secret_provider_contributors(&config);

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
    ir.tasks.retain(|task| {
        task.phase.is_some()
            && (credentials == Credentials::SetUp || !grants_credentials(task, &secret_providers))
    });
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

    type TestResult<Success = ()> = std::result::Result<Success, Box<dyn std::error::Error>>;

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
            config,
        };
        Fixture {
            _module: module,
            info,
        }
    }

    fn workflows(workflows: &serde_json::Value) -> TestResult<Vec<serde_yaml::Value>> {
        parse(&generate_workflows(&project(workflows).info, "cuenv")?)
    }

    fn parse(generated: &[(String, String)]) -> TestResult<Vec<serde_yaml::Value>> {
        let names: Vec<&str> = generated.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "dns-infrastructure-diff.yml",
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
    fn projects_without_workflows_generate_nothing() -> TestResult {
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
    fn the_retired_plan_setting_names_its_replacement() {
        let fixture = project(&serde_json::json!({"plan": {"comment": false}}));
        let error = generate_workflows(&fixture.info, "cuenv").expect_err("retired plan");
        let message = error.to_string();
        assert!(
            message.contains("renamed to infrastructure.workflows.diff"),
            "{message}"
        );
        assert!(stale_workflows(&fixture.info, "cuenv").is_err());
    }

    #[test]
    fn defaults_diff_and_comment_on_pull_requests_and_apply_on_main() -> TestResult {
        let generated = workflows(&serde_json::json!({}))?;
        let (diff, apply) = (&generated[0], &generated[1]);

        let diff_paths: Vec<&str> = diff["on"]["pull_request"]["paths"]
            .as_sequence()
            .ok_or("diff paths")?
            .iter()
            .filter_map(serde_yaml::Value::as_str)
            .collect();
        for path in [
            ".github/workflows/dns-infrastructure-diff.yml",
            "cue.mod/**",
            "infrastructure/dns/**",
            "infrastructure/dns/env.cue",
            "shared/shared.cue",
        ] {
            assert!(diff_paths.contains(&path), "{path} in {diff_paths:?}");
        }
        assert_eq!(
            apply["on"]["push"]["paths"].as_sequence().map(Vec::len),
            Some(diff_paths.len())
        );
        // Files under a derived directory glob are not listed again.
        assert!(
            !diff_paths.contains(&"infrastructure/dns/zones/zones.cue"),
            "{diff_paths:?}"
        );
        assert!(diff["on"].get("push").is_none());
        assert_eq!(diff["concurrency"]["cancel-in-progress"], true);
        let job = &diff["jobs"]["dns-infrastructure-diff"];
        assert_eq!(job["permissions"]["pull-requests"], "write");
        assert!(job.get("environment").is_none());
        let diff_steps = steps(diff, "dns-infrastructure-diff");
        let checkout = step(&diff_steps, "Checkout").ok_or("checkout step")?;
        assert_eq!(checkout["with"]["fetch-depth"], 0);
        let diff_step = step(&diff_steps, "Diff infrastructure").ok_or("diff step")?;
        assert_eq!(diff_step["id"], "diff");
        let run = diff_step["run"].as_str().ok_or("diff run")?;
        assert!(
            run.starts_with("cuenv infra init --path infrastructure/dns || exit $?\n"),
            "{run}"
        );
        assert!(
            run.contains(
                "cuenv infrastructure diff --base \"origin/$GITHUB_BASE_REF\" --path \
                 infrastructure/dns > \"${{ runner.temp }}/cuenv-infrastructure-diff.md\" 2> \
                 \"${{ runner.temp }}/cuenv-infrastructure-diff-errors.txt\" || status=$?\n"
            ),
            "{run}"
        );
        assert!(
            run.contains("printf '%s\\n\\n' '### Infrastructure changes: `dns`'"),
            "{run}"
        );
        assert!(run.contains("$GITHUB_STEP_SUMMARY"), "{run}");
        assert!(run.ends_with("exit \"$status\""), "{run}");
        assert!(!run.contains("infrastructure plan"), "{run}");
        let comment = step(&diff_steps, "Comment infrastructure changes").ok_or("comment step")?;
        assert_eq!(comment["uses"], "actions/github-script@v7");
        assert_eq!(comment["env"]["DIFF_OUTCOME"], "${{ steps.diff.outcome }}");
        let script = comment["with"]["script"].as_str().ok_or("script")?;
        assert!(
            script.contains("const marker = \"<!-- cuenv-infrastructure-diff:dns -->\";"),
            "{script}"
        );
        assert!(
            script.contains("const retiredMarker = \"<!-- cuenv-infrastructure-plan:dns -->\";"),
            "{script}"
        );
        assert!(script.contains("const project = \"dns\";"), "{script}");
        assert!(script.contains("'Infrastructure changes'"), "{script}");
        assert!(script.contains("updateComment"), "{script}");

        assert_eq!(apply["on"]["push"]["branches"][0], "main");
        assert!(apply["on"].get("workflow_dispatch").is_some());
        assert!(apply["on"].get("pull_request").is_none());
        assert_eq!(apply["concurrency"]["cancel-in-progress"], false);
        let apply_steps = steps(apply, "dns-infrastructure-apply");
        let apply_checkout = step(&apply_steps, "Checkout").ok_or("apply checkout")?;
        assert_eq!(apply_checkout["with"]["fetch-depth"], 2);
        let apply_step = step(&apply_steps, "Apply infrastructure").ok_or("apply step")?;
        assert_eq!(
            apply_step["run"],
            "cuenv infra init --path infrastructure/dns &&\ncuenv infrastructure apply --yes --path infrastructure/dns"
        );
        assert!(step(&apply_steps, "Comment infrastructure changes").is_none());
        Ok(())
    }

    #[test]
    fn diff_comment_can_be_disabled() -> TestResult {
        let generated = workflows(&serde_json::json!({"diff": {"comment": false}}))?;
        let diff_steps = steps(&generated[0], "dns-infrastructure-diff");
        assert!(step(&diff_steps, "Diff infrastructure").is_some());
        assert!(step(&diff_steps, "Comment infrastructure changes").is_none());
        Ok(())
    }

    /// Contributors like the built-in ones: cuenv itself, 1Password and
    /// Infisical (activated by the secret providers in use), an OIDC token for
    /// Infisical, and an always-on step that asks for an OIDC token.
    fn contributors() -> serde_json::Value {
        serde_json::json!({
            "contributors": [
                {
                    "id": "cuenv",
                    "when": {"always": true},
                    "tasks": [{"id": "cuenv.setup", "label": "Setup cuenv", "command": "echo", "args": ["install-cuenv"], "priority": 10}]
                },
                {
                    "id": "1password",
                    "when": {"secretsProvider": ["onepassword"]},
                    "tasks": [{
                        "id": "1password.setup", "label": "Setup 1Password", "command": "cuenv",
                        "args": ["secrets", "setup", "onepassword"], "priority": 20,
                        "env": {"OP_SERVICE_ACCOUNT_TOKEN": "${OP_SERVICE_ACCOUNT_TOKEN}"}
                    }]
                },
                {
                    "id": "infisical-oidc",
                    "when": {"secretsProvider": ["infisical"]},
                    "tasks": [{
                        "id": "infisical.oidc", "label": "Get GitHub OIDC token for Infisical",
                        "script": "echo oidc", "priority": 15,
                        "ci": {"github": {"permissions": {"id-token": "write"}}}
                    }]
                },
                {
                    "id": "infisical-secrets-action",
                    "when": {"always": true},
                    "tasks": [{
                        "id": "infisical.secrets-action", "label": "Fetch secrets from Infisical",
                        "command": "sh", "args": ["-c", "exit 1"], "priority": 15,
                        "ci": {"github": {"permissions": {"id-token": "write"}}}
                    }]
                }
            ]
        })
    }

    #[test]
    fn the_diff_job_sets_up_no_secret_provider_and_asks_for_no_credential() -> TestResult {
        let mut fixture = project(&serde_json::json!({}));
        fixture.info.config.ci = Some(serde_json::from_value(contributors())?);
        fixture.info.config.infrastructure = Some(serde_json::json!({
            "state": {"d1": {
                "accountId": "0123456789abcdef0123456789abcdef",
                "databaseId": "0b2c3d4e-5f60-4718-8293-a4b5c6d7e8f9",
                "token": {"resolver": "onepassword", "ref": "op://ci/d1/token"},
            }},
            "resources": {"record": {
                "type": "cloudflare_dns_record",
                "configuration": {"content": {"resolver": "infisical", "projectId": "p", "environment": "prod", "secretName": "IP"}},
            }},
            "workflows": {},
        }));
        let generated = generate_workflows(&fixture.info, "cuenv")?;
        let (diff_yaml, apply_yaml) = (&generated[0].1, &generated[1].1);
        for credential in [
            "OP_SERVICE_ACCOUNT_TOKEN",
            "secrets setup",
            "Setup 1Password",
            "Infisical",
            "id-token",
        ] {
            assert!(!diff_yaml.contains(credential), "{credential}: {diff_yaml}");
            assert!(
                apply_yaml.contains(credential),
                "{credential}: {apply_yaml}"
            );
        }
        let workflows = parse(&generated)?;
        let diff_steps = steps(&workflows[0], "dns-infrastructure-diff");
        let names: Vec<&str> = diff_steps
            .iter()
            .filter_map(|step| step["name"].as_str())
            .collect();
        assert_eq!(
            names,
            [
                "Checkout",
                "Setup cuenv",
                "Diff infrastructure",
                "Comment infrastructure changes"
            ]
        );
        let permissions = &workflows[0]["jobs"]["dns-infrastructure-diff"]["permissions"];
        assert!(permissions.get("id-token").is_none(), "{permissions:?}");
        assert_eq!(
            workflows[1]["jobs"]["dns-infrastructure-apply"]["permissions"]["id-token"],
            "write"
        );
        Ok(())
    }

    #[test]
    fn apply_on_tag_triggers_on_tag_pushes() -> TestResult {
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
    fn apply_on_release_triggers_on_published_releases() -> TestResult {
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
        // The diff job needs no approval.
        assert!(
            generated[0]["jobs"]["dns-infrastructure-diff"]
                .get("environment")
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn apply_on_manual_runs_only_manually() -> TestResult {
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
    fn apply_on_push_runs_only_for_the_branch_and_queues_every_ref() -> TestResult {
        let generated = workflows(&serde_json::json!({"apply": {"branch": "it's"}}))?;
        let apply = &generated[1];
        assert_eq!(apply["concurrency"]["group"], "${{ github.workflow }}");
        assert_eq!(
            apply["jobs"]["dns-infrastructure-apply"]["if"],
            "github.ref == 'refs/heads/it''s'"
        );
        let diff = &generated[0];
        assert!(diff["jobs"]["dns-infrastructure-diff"].get("if").is_none());
        Ok(())
    }

    #[test]
    fn comment_skips_cancelled_runs_and_foreign_comments() -> TestResult {
        let generated = workflows(&serde_json::json!({}))?;
        let diff_steps = steps(&generated[0], "dns-infrastructure-diff");
        let comment = step(&diff_steps, "Comment infrastructure changes").ok_or("comment step")?;
        let condition = comment["if"].as_str().ok_or("condition")?;
        assert!(condition.starts_with("!cancelled() && "), "{condition}");
        assert!(
            condition.contains("steps.diff.outcome != 'skipped'"),
            "{condition}"
        );
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
    fn names_in_the_comment_script_are_javascript_strings() {
        let step = diff_comment_step("it's \"quoted\"");
        let script = step
            .with_inputs
            .get("script")
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            script.contains(r#"const project = "it's \"quoted\"";"#),
            "{script}"
        );
        assert!(!script.contains("__"), "{script}");
    }

    #[test]
    fn separate_infrastructure_projects_generate_workflows_and_cover_sources() -> TestResult {
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
        // Only the apply job resolves the state credential.
        assert!(
            !generated[0].1.contains("prepare-state-secrets"),
            "{}",
            generated[0].1
        );
        assert!(
            generated[1].1.contains("prepare-state-secrets"),
            "{}",
            generated[1].1
        );
        let diff: serde_yaml::Value = serde_yaml::from_str(&generated[0].1)?;
        let paths = diff["on"]["pull_request"]["paths"]
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
        assert_eq!(
            stale_workflows(&fixture.info, "cuenv")?,
            ["dns-infrastructure-plan.yml"]
        );
        std::fs::write(
            directory.join("main.cue"),
            "package stack\ninfrastructure: resources: pending: configuration: name: string\n",
        )?;
        assert!(generate_workflows(&fixture.info, "cuenv")?.is_empty());
        assert_eq!(stale_workflows(&fixture.info, "cuenv")?.len(), 3);
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
    fn locator_workflows_belong_to_the_explicit_project_package() -> TestResult {
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
        for name in [DIFF_PIPELINE, APPLY_PIPELINE] {
            let mut fixture = project(&serde_json::json!({}));
            let mut ci = CI::default();
            ci.pipelines.insert(name.to_string(), Pipeline::default());
            fixture.info.config.ci = Some(ci);
            let error = generate_workflows(&fixture.info, "cuenv").expect_err("name collision");
            assert!(error.to_string().contains(name), "{error}");
        }
    }

    #[test]
    fn disabling_workflows_marks_their_files_stale() -> TestResult {
        let mut fixture = project(&serde_json::json!({}));
        let info = &mut fixture.info;
        info.config.name = "rawkode.academy".to_string();
        // The retired plan workflow is stale whenever there is infrastructure.
        assert_eq!(
            stale_workflows(info, "cuenv")?,
            ["rawkodeacademy-infrastructure-plan.yml"]
        );
        info.config.infrastructure = Some(serde_json::json!({"state": {}}));
        assert_eq!(
            stale_workflows(info, "cuenv")?,
            [
                "rawkodeacademy-infrastructure-diff.yml",
                "rawkodeacademy-infrastructure-apply.yml",
                "rawkodeacademy-infrastructure-plan.yml"
            ]
        );
        info.config.infrastructure = None;
        assert!(stale_workflows(info, "cuenv")?.is_empty());
        Ok(())
    }

    #[test]
    fn a_workflow_file_of_a_ci_pipeline_is_never_stale() -> TestResult {
        let mut fixture = project(&serde_json::json!({}));
        let mut ci = CI::default();
        ci.pipelines
            .insert(RETIRED_PLAN_PIPELINE.to_string(), Pipeline::default());
        fixture.info.config.ci = Some(ci);
        assert!(stale_workflows(&fixture.info, "cuenv")?.is_empty());
        fixture.info.config.infrastructure = Some(serde_json::json!({"state": {}}));
        assert_eq!(
            stale_workflows(&fixture.info, "cuenv")?,
            [
                "dns-infrastructure-diff.yml",
                "dns-infrastructure-apply.yml"
            ]
        );
        Ok(())
    }
}
