use super::*;
use cuenv_manifest::environment::ContextEnv;
use serde_json::from_value;

fn facts(ci: Option<CiFacts>) -> Facts {
    Facts {
        ci,
        user_name: Some("Ada".to_string()),
        user_email: Some("ada@example.com".to_string()),
        host_os: "linux".to_string(),
        host_arch: "x86_64".to_string(),
        host_name: Some("workstation".to_string()),
        branch: Some("main".to_string()),
    }
}

fn pull_request() -> Option<CiFacts> {
    Some(CiFacts {
        provider: "github".to_string(),
        event: Some("pull_request".to_string()),
        reference: Some("refs/pull/1/merge".to_string()),
        actor: Some("ada".to_string()),
    })
}

fn definition(filter: Value) -> ContextDefinition {
    from_value(filter).unwrap()
}

fn definitions(filters: &[(&str, Value)]) -> HashMap<String, ContextDefinition> {
    filters
        .iter()
        .map(|(name, filter)| ((*name).to_string(), definition(filter.clone())))
        .collect()
}

fn task<'a>(name: &'a str, command: Option<&'a str>, labels: &'a [String]) -> Consumer<'a> {
    Consumer::Task(TaskSubject {
        name,
        command,
        labels,
    })
}

fn variables(pairs: &[(&str, &str)]) -> HashMap<String, EnvValue> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), EnvValue::String((*value).to_string())))
        .collect()
}

fn context(pairs: &[(&str, &str)]) -> ContextEnv {
    ContextEnv {
        environment: None,
        base: variables(pairs),
    }
}

fn plan_and_apply() -> HashMap<String, ContextDefinition> {
    definitions(&[
        ("plan", json!({"infrastructure": "plan"})),
        ("apply", json!({"infrastructure": "apply|import|destroy"})),
    ])
}

fn plan_and_apply_env() -> Env {
    Env {
        base: variables(&[("REGION", "eu")]),
        environment: Some(HashMap::from([(
            "production".to_string(),
            variables(&[("REGION", "us")]),
        )])),
        context: Some(HashMap::from([
            ("plan".to_string(), context(&[("TOKEN", "read-only")])),
            (
                "apply".to_string(),
                ContextEnv {
                    environment: Some(HashMap::from([(
                        "production".to_string(),
                        variables(&[("TOKEN", "production-read-write")]),
                    )])),
                    base: variables(&[("TOKEN", "read-write")]),
                },
            ),
        ])),
    }
}

fn select(
    env: &Env,
    environment: Option<&str>,
    consumer: Consumer<'_>,
) -> HashMap<String, EnvValue> {
    Selection {
        env,
        definitions: &plan_and_apply(),
        environment,
        consumer,
        directory: Path::new("."),
    }
    .variables_with(&facts(None))
    .unwrap()
}

#[test]
fn ci_facts_are_absent_outside_ci() {
    assert_eq!(CiFacts::from_variables(|_| None), None);
    assert_eq!(
        CiFacts::from_variables(|name| (name == "CI").then(|| "false".to_string())),
        None
    );
}

#[test]
fn ci_facts_read_github_variables() {
    let ci = CiFacts::from_variables(|name| match name {
        "GITHUB_ACTIONS" => Some("true".to_string()),
        "GITHUB_EVENT_NAME" => Some("pull_request".to_string()),
        "GITHUB_REF" => Some("refs/pull/1/merge".to_string()),
        "GITHUB_ACTOR" => Some("ada".to_string()),
        _ => None,
    });
    assert_eq!(ci, pull_request());
}

#[test]
fn ci_facts_name_an_unrecognised_provider_unknown() {
    let ci = CiFacts::from_variables(|name| (name == "CI").then(|| "true".to_string())).unwrap();
    assert_eq!(ci.provider, "unknown");
    assert_eq!(ci.event, None);
}

#[test]
fn document_has_every_fact_for_every_consumer() {
    let document = facts(None).document(task("deploy.production", Some("bun"), &[]));
    assert_eq!(document["consumer"], "task");
    assert_eq!(document["exec"], Value::Null);
    assert_eq!(document["infrastructure"], Value::Null);
    assert_eq!(document["ci"], Value::Null);
    assert_eq!(document["user"]["email"], "ada@example.com");
    assert_eq!(document["vcs"]["branch"], "main");

    let document =
        facts(pull_request()).document(Consumer::Infrastructure(InfrastructurePolicyAction::Plan));
    assert_eq!(document["consumer"], "infrastructure");
    assert_eq!(document["infrastructure"], "plan");
    assert_eq!(document["ci"]["event"], "pull_request");
    assert_eq!(document["ci"]["ref"], "refs/pull/1/merge");
}

#[test]
fn the_fields_of_an_entry_decide_its_filter() {
    assert!(matches!(
        definition(json!({"command": "bun"})),
        ContextDefinition::Task(_)
    ));
    assert!(matches!(
        definition(json!({"name": "deploy.*", "labels": ["release"]})),
        ContextDefinition::Task(_)
    ));
    assert!(matches!(
        definition(json!({"ci": null})),
        ContextDefinition::When(_)
    ));
    assert!(matches!(
        definition(json!({"consumer": "exec", "exec": "bun"})),
        ContextDefinition::When(_)
    ));
}

fn active(filter: Value, facts: &Facts, consumer: Consumer<'_>) -> bool {
    let definitions = definitions(&[("context", filter)]);
    !active_contexts(&definitions, facts, consumer)
        .unwrap()
        .is_empty()
}

#[test]
fn a_string_must_match_the_whole_value() {
    let local = facts(None);
    for (filter, program, expected) in [
        ("bun", "bun", true),
        ("bun", "bunx", false),
        ("bun", "ubuntu", false),
        ("bun.*", "bunx", true),
        ("bun|bunx", "bunx", true),
        ("^bun$", "bun", true),
        ("g\\+\\+", "g++", true),
    ] {
        assert_eq!(
            active(json!({"exec": filter}), &local, Consumer::Exec(program)),
            expected,
            "{filter} against {program}"
        );
    }
}

#[test]
fn a_when_matches_the_facts_it_names() {
    let labels = [];
    let local_task = (facts(None), task("deploy.production", Some("bun"), &labels));
    let ci_exec = (facts(pull_request()), Consumer::Exec("wrangler"));
    let cases = [
        (json!({"ci": null}), true, false),
        (json!({"consumer": "task"}), true, false),
        (json!({"consumer": "task|exec"}), true, true),
        (json!({"exec": null}), true, false),
        (json!({"exec": "wrangler"}), false, true),
        (json!({"ci": {"event": "pull_request"}}), false, true),
        (json!({"ci": {"event": "push"}}), false, false),
        (
            json!({"ci": {"provider": "github"}, "exec": "w.*"}),
            false,
            true,
        ),
        (json!({"user": {"email": ".*@example\\.com"}}), true, true),
        (
            json!({"vcs": {"branch": "main"}, "host": {"os": "linux"}}),
            true,
            true,
        ),
        (json!({"vcs": {"branch": null}}), false, false),
    ];
    for (filter, expect_local_task, expect_ci_exec) in cases {
        assert_eq!(
            active(filter.clone(), &local_task.0, local_task.1),
            expect_local_task,
            "{filter}"
        );
        assert_eq!(
            active(filter.clone(), &ci_exec.0, ci_exec.1),
            expect_ci_exec,
            "{filter}"
        );
    }
}

#[test]
fn a_task_filter_matches_one_task() {
    let local = facts(None);
    let release = ["release".to_string(), "docs".to_string()];
    let cases = [
        (json!({"name": "deploy"}), false),
        (json!({"name": "deploy.*"}), true),
        (json!({"name": "deploy\\.production"}), true),
        (json!({"command": "bun"}), true),
        (json!({"command": "bunx"}), false),
        (json!({"labels": ["release"]}), true),
        (json!({"labels": ["release", "docs"]}), true),
        (json!({"labels": ["release", "nightly"]}), false),
        (json!({"labels": ["rel.*"]}), true),
        (
            json!({"name": "deploy.*", "command": "bun", "labels": ["docs"]}),
            true,
        ),
        (json!({"name": "deploy.*", "command": "npm"}), false),
    ];
    for (filter, expected) in cases {
        assert_eq!(
            active(
                filter.clone(),
                &local,
                task("deploy.production", Some("bun"), &release)
            ),
            expected,
            "{filter}"
        );
    }
}

#[test]
fn a_task_filter_matches_nothing_but_a_task() {
    let local = facts(None);
    let command = json!({"command": "bun"});
    assert!(!active(command.clone(), &local, Consumer::Exec("bun")));
    assert!(!active(command.clone(), &local, Consumer::Shell));
    // A script task runs no program.
    assert!(!active(command, &local, task("build", None, &[])));
    assert!(active(
        json!({"name": "build"}),
        &local,
        task("build", None, &[])
    ));
}

#[test]
fn active_contexts_are_sorted() {
    let mut definitions = plan_and_apply();
    definitions.insert("aLocal".to_string(), definition(json!({"ci": null})));
    assert_eq!(
        active_contexts(
            &definitions,
            &facts(None),
            Consumer::Infrastructure(InfrastructurePolicyAction::Apply)
        )
        .unwrap(),
        ["aLocal", "apply"]
    );
}

#[test]
fn a_context_variable_exists_only_while_its_context_is_active() {
    let env = plan_and_apply_env();
    let plan = select(
        &env,
        None,
        Consumer::Infrastructure(InfrastructurePolicyAction::Plan),
    );
    assert_eq!(plan, variables(&[("REGION", "eu"), ("TOKEN", "read-only")]));

    let apply = select(
        &env,
        None,
        Consumer::Infrastructure(InfrastructurePolicyAction::Apply),
    );
    assert_eq!(
        apply,
        variables(&[("REGION", "eu"), ("TOKEN", "read-write")])
    );

    let build = select(&env, None, task("build", Some("cargo"), &[]));
    assert_eq!(build, variables(&[("REGION", "eu")]));
}

#[test]
fn a_context_environment_overrides_the_context() {
    let env = plan_and_apply_env();
    let apply = select(
        &env,
        Some("production"),
        Consumer::Infrastructure(InfrastructurePolicyAction::Apply),
    );
    assert_eq!(
        apply,
        variables(&[("REGION", "us"), ("TOKEN", "production-read-write")])
    );
    let plan = select(
        &env,
        Some("production"),
        Consumer::Infrastructure(InfrastructurePolicyAction::Plan),
    );
    assert_eq!(plan, variables(&[("REGION", "us"), ("TOKEN", "read-only")]));
}

#[test]
fn a_context_variable_may_not_be_set_outside_a_context() {
    let mut env = plan_and_apply_env();
    env.base
        .insert("TOKEN".to_string(), EnvValue::String("root".to_string()));
    let error = validate(&env, &plan_and_apply()).unwrap_err().to_string();
    assert!(error.contains("TOKEN is set under env.context."), "{error}");

    let mut env = plan_and_apply_env();
    env.environment = Some(HashMap::from([(
        "production".to_string(),
        variables(&[("TOKEN", "root")]),
    )]));
    assert!(validate(&env, &plan_and_apply()).is_err());
}

#[test]
fn overlapping_contexts_may_not_set_the_same_variable() {
    let mut definitions = plan_and_apply();
    definitions.insert("local".to_string(), definition(json!({"ci": null})));
    let mut env = plan_and_apply_env();
    let contexts = env.context.as_mut().unwrap();
    contexts.insert("local".to_string(), context(&[("EDITOR", "hx")]));
    assert!(validate(&env, &definitions).is_ok());

    let contexts = env.context.as_mut().unwrap();
    contexts.insert("local".to_string(), context(&[("TOKEN", "root")]));
    let error = validate(&env, &definitions).unwrap_err().to_string();
    assert!(error.contains("can both be active"), "{error}");
}

#[test]
fn contexts_are_exclusive_only_when_that_is_proved() {
    let cases = [
        // The values of `infrastructure` are known, so expressions are compared.
        (
            json!({"infrastructure": "plan"}),
            json!({"infrastructure": "apply|destroy"}),
            true,
        ),
        (
            json!({"infrastructure": "plan|apply"}),
            json!({"infrastructure": "apply"}),
            false,
        ),
        (
            json!({"infrastructure": "state-.*"}),
            json!({"infrastructure": "plan"}),
            true,
        ),
        // `exec` and `infrastructure` are null for every other consumer.
        (
            json!({"infrastructure": "plan"}),
            json!({"exec": "terraform"}),
            true,
        ),
        (
            json!({"infrastructure": "plan"}),
            json!({"consumer": "task|exec"}),
            true,
        ),
        (
            json!({"infrastructure": null}),
            json!({"consumer": "infrastructure"}),
            true,
        ),
        (json!({"exec": "bun"}), json!({"name": "deploy"}), true),
        (
            json!({"consumer": "shell"}),
            json!({"command": "bun"}),
            true,
        ),
        (json!({"ci": null}), json!({"ci": {"event": "push"}}), true),
        (json!({"ci": null}), json!({"command": "bun"}), false),
        (
            json!({"ci": null}),
            json!({"vcs": {"branch": "main"}}),
            false,
        ),
        // A plain value is compared with the other expression.
        (
            json!({"vcs": {"branch": "main"}}),
            json!({"vcs": {"branch": "release"}}),
            true,
        ),
        (
            json!({"vcs": {"branch": "main"}}),
            json!({"vcs": {"branch": "ma.*"}}),
            false,
        ),
        (
            json!({"vcs": {"branch": "main"}}),
            json!({"vcs": {"branch": "release-.*"}}),
            true,
        ),
        (
            json!({"vcs": {"branch": "a.*"}}),
            json!({"vcs": {"branch": "b.*"}}),
            false,
        ),
        (json!({"name": "build"}), json!({"name": "test"}), true),
        (json!({"name": "build"}), json!({"name": "build.*"}), false),
        (json!({"command": "bun"}), json!({"command": "npm"}), true),
        (json!({"command": "bun"}), json!({"name": "deploy"}), false),
        (json!({"labels": ["a"]}), json!({"labels": ["b"]}), false),
    ];
    for (first, second, expected) in cases {
        let (one, other) = (definition(first.clone()), definition(second.clone()));
        assert_eq!(
            exclusive(&one, &other).unwrap(),
            expected,
            "{first} {second}"
        );
        assert_eq!(
            exclusive(&other, &one).unwrap(),
            expected,
            "{second} {first}"
        );
    }
}

#[test]
fn a_context_must_be_defined() {
    let mut env = plan_and_apply_env();
    let contexts = env.context.as_mut().unwrap();
    contexts.insert("deploy".to_string(), context(&[("WRANGLER_TOKEN", "x")]));
    let error = validate(&env, &plan_and_apply()).unwrap_err().to_string();
    assert!(error.contains("define contexts.deploy"), "{error}");
}

#[test]
fn contexts_are_checked_before_any_variable_uses_them() {
    let env = Env {
        base: variables(&[("REGION", "eu")]),
        environment: None,
        context: None,
    };
    let definitions = definitions(&[("plan", json!({"infrastructure": "aply"}))]);
    let error = Selection {
        env: &env,
        definitions: &definitions,
        environment: None,
        consumer: Consumer::Shell,
        directory: Path::new("/nonexistent"),
    }
    .variables()
    .unwrap_err()
    .to_string();
    assert!(error.contains("contexts.plan:"), "{error}");
}

#[test]
fn an_env_without_contexts_needs_no_facts() {
    let env = Env {
        base: variables(&[("REGION", "eu")]),
        environment: None,
        context: None,
    };
    let selected = Selection {
        env: &env,
        definitions: &HashMap::new(),
        environment: None,
        consumer: Consumer::Shell,
        directory: Path::new("/nonexistent"),
    }
    .variables()
    .unwrap();
    assert_eq!(selected, variables(&[("REGION", "eu")]));
}

#[test]
fn an_invalid_filter_is_an_error() {
    let env = plan_and_apply_env();
    for (filter, expected) in [
        (json!({"tsak": "deploy"}), "`tsak` is not a fact"),
        (json!({"ci": {"evnt": "push"}}), "`ci.evnt` is not a fact"),
        (
            json!({"exec": {"name": "bun"}}),
            "`exec.name` is not a fact",
        ),
        (json!({"exec": "("}), "is not a valid regular expression"),
        // Would otherwise close the anchoring group and match everything.
        (
            json!({"name": "deploy)|(.*"}),
            "is not a valid regular expression",
        ),
        (
            json!({"name": "deploy("}),
            "is not a valid regular expression",
        ),
        (
            json!({"labels": ["["]}),
            "is not a valid regular expression",
        ),
        (
            json!({"infrastructure": "aply"}),
            "`aply` matches no value of `infrastructure`",
        ),
        (
            json!({"consumer": "tasks"}),
            "`tasks` matches no value of `consumer`",
        ),
        (json!({"host": {"os": 1}}), "`host.os` must be a string"),
        (json!({}), "the filter is empty"),
    ] {
        let mut definitions = plan_and_apply();
        definitions.insert("typo".to_string(), definition(filter));
        let error = validate(&env, &definitions).unwrap_err().to_string();
        assert!(error.contains("contexts.typo:"), "{error}");
        assert!(error.contains(expected), "{error}");
    }
}

fn override_definitions() -> HashMap<String, ContextDefinition> {
    definitions(&[
        ("plan", json!({"infrastructure": "plan"})),
        ("apply", json!({"infrastructure": "apply"})),
        ("local", json!({"ci": null})),
    ])
}

#[test]
fn an_override_applies_only_while_its_context_is_active() {
    let definitions = override_definitions();
    let overrides = Overrides {
        field: "infrastructure.state.s3.context",
        names: &["apply", "plan"],
        definitions: &definitions,
    };
    let active = |action| {
        overrides
            .active(&facts(None), Consumer::Infrastructure(action))
            .unwrap()
    };
    assert_eq!(active(InfrastructurePolicyAction::Apply), Some("apply"));
    assert_eq!(active(InfrastructurePolicyAction::Plan), Some("plan"));
    assert_eq!(active(InfrastructurePolicyAction::StateList), None);
}

#[test]
fn overrides_reject_undefined_and_overlapping_contexts() {
    let definitions = override_definitions();
    let consumer = Consumer::Infrastructure(InfrastructurePolicyAction::Plan);
    let undefined = Overrides {
        field: "infrastructure.state.s3.context",
        names: &["deploy"],
        definitions: &definitions,
    }
    .active(&facts(None), consumer)
    .unwrap_err()
    .to_string();
    assert!(undefined.contains("define contexts.deploy"), "{undefined}");
    // Rejected even though only `local` matches this invocation.
    let overlapping = Overrides {
        field: "infrastructure.state.s3.context",
        names: &["apply", "local"],
        definitions: &definitions,
    }
    .active(&facts(None), consumer)
    .unwrap_err()
    .to_string();
    assert!(overlapping.contains("can both be active"), "{overlapping}");
}

#[test]
fn facts_are_read_from_the_repository_without_running_git() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    gix::init(directory.path()).expect("a repository");
    let git = directory.path().join(".git");
    std::fs::write(git.join("HEAD"), "ref: refs/heads/feature/contexts\n").expect("HEAD");
    std::fs::write(
        git.join("config"),
        "[core]\n\trepositoryformatversion = 0\n[user]\n\tname = Ada\n\temail = ada@example.com\n",
    )
    .expect("config");

    let facts = temp_env::with_var("PATH", Some(""), || Facts::detect(directory.path()));

    assert_eq!(facts.user_name.as_deref(), Some("Ada"));
    assert_eq!(facts.user_email.as_deref(), Some("ada@example.com"));
    assert_eq!(facts.branch.as_deref(), Some("feature/contexts"));
    #[cfg(unix)]
    assert!(facts.host_name.is_some());
}

#[test]
fn a_directory_outside_a_repository_has_no_git_facts() {
    let directory = tempfile::tempdir().expect("a temporary directory");

    let facts = temp_env::with_var("GIT_CEILING_DIRECTORIES", directory.path().parent(), || {
        Facts::detect(directory.path())
    });

    assert_eq!(facts.branch, None);
}
