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

fn matcher(value: Value) -> ContextMatcher {
    from_value(value).unwrap()
}

fn definition(when: Value, overlaps: &[&str]) -> ContextDefinition {
    ContextDefinition {
        when: matcher(when),
        overlaps: overlaps.iter().map(ToString::to_string).collect(),
    }
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
    HashMap::from([
        (
            "plan".to_string(),
            definition(
                json!({"fields": {"infrastructure": {"equals": "plan"}}}),
                &[],
            ),
        ),
        (
            "apply".to_string(),
            definition(
                json!({"fields": {"infrastructure": {"anyOf": [
                    {"equals": "apply"}, {"equals": "import"}, {"equals": "destroy"},
                ]}}}),
                &[],
            ),
        ),
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
fn document_has_every_field_for_every_consumer() {
    let document = facts(None).document(Consumer::Task("deploy.production"));
    assert_eq!(document["command"], "task");
    assert_eq!(document["task"], "deploy.production");
    assert_eq!(document["exec"], Value::Null);
    assert_eq!(document["infrastructure"], Value::Null);
    assert_eq!(document["ci"], Value::Null);
    assert_eq!(document["user"]["email"], "ada@example.com");
    assert_eq!(document["vcs"]["branch"], "main");

    let document =
        facts(pull_request()).document(Consumer::Infrastructure(InfrastructurePolicyAction::Plan));
    assert_eq!(document["command"], "infrastructure");
    assert_eq!(document["infrastructure"], "plan");
    assert_eq!(document["ci"]["event"], "pull_request");
    assert_eq!(document["ci"]["ref"], "refs/pull/1/merge");
}

#[test]
fn matchers_follow_cue_semantics() {
    let local = facts(None).document(Consumer::Task("deploy.production"));
    let ci = facts(pull_request()).document(Consumer::Exec("wrangler"));
    let cases = [
        (json!({"fields": {"ci": {"equals": null}}}), true, false),
        (
            json!({"fields": {"task": {"matches": "^deploy"}}}),
            true,
            false,
        ),
        (
            json!({"fields": {"task": {"notMatches": "^deploy"}}}),
            false,
            false,
        ),
        (json!({"fields": {"exec": {"kind": "string"}}}), false, true),
        (
            json!({"fields": {"ci": {"fields": {"event": {"equals": "pull_request"}}}}}),
            false,
            true,
        ),
        // `!=` holds only for a value of the same kind, so `null` does not
        // satisfy `event: !="push"`.
        (
            json!({"fields": {"ci": {"fields": {"event": {"notEquals": "push"}}}}}),
            false,
            true,
        ),
        (
            json!({"anyOf": [
                {"fields": {"ci": {"equals": null}}},
                {"fields": {"exec": {"equals": "wrangler"}}},
            ]}),
            true,
            true,
        ),
        (
            json!({"allOf": [
                {"fields": {"command": {"equals": "task"}}},
                {"fields": {"user": {"fields": {"email": {"matches": "@example\\.com$"}}}}},
            ]}),
            true,
            false,
        ),
        (
            json!({"fields": {"unknown": {"kind": "any"}}}),
            false,
            false,
        ),
    ];
    for (constraint, expect_local, expect_ci) in cases {
        let compiled = matcher(constraint.clone());
        assert_eq!(
            matches(&compiled, &local).unwrap(),
            expect_local,
            "{constraint}"
        );
        assert_eq!(matches(&compiled, &ci).unwrap(), expect_ci, "{constraint}");
    }
}

#[test]
fn an_invalid_regular_expression_is_an_error() {
    let document = facts(None).document(Consumer::Task("build"));
    let compiled = matcher(json!({"fields": {"task": {"matches": "("}}}));
    assert!(matches(&compiled, &document).is_err());
}

#[test]
fn active_contexts_are_sorted() {
    let document =
        facts(None).document(Consumer::Infrastructure(InfrastructurePolicyAction::Apply));
    let mut definitions = plan_and_apply();
    definitions.insert(
        "aLocal".to_string(),
        definition(
            json!({"fields": {"ci": {"equals": null}}}),
            &["apply", "plan"],
        ),
    );
    assert_eq!(
        active_contexts(&definitions, &document).unwrap(),
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

    let task = select(&env, None, Consumer::Task("build"));
    assert_eq!(task, variables(&[("REGION", "eu")]));
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
    definitions.insert(
        "local".to_string(),
        definition(
            json!({"fields": {"ci": {"equals": null}}}),
            &["apply", "plan"],
        ),
    );
    for name in ["plan", "apply"] {
        definitions.get_mut(name).unwrap().overlaps = vec!["local".to_string()];
    }
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
fn a_context_must_be_defined() {
    let mut env = plan_and_apply_env();
    let contexts = env.context.as_mut().unwrap();
    contexts.insert("deploy".to_string(), context(&[("WRANGLER_TOKEN", "x")]));
    let error = validate(&env, &plan_and_apply()).unwrap_err().to_string();
    assert!(error.contains("define contexts.deploy.when"), "{error}");
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
fn a_constraint_over_an_unknown_fact_is_an_error() {
    let env = plan_and_apply_env();
    for (constraint, expected) in [
        (
            json!({"fields": {"tsak": {"equals": "deploy"}}}),
            "`tsak` is not a fact",
        ),
        (
            json!({"fields": {"ci": {"anyOf": [
                {"equals": null},
                {"fields": {"evnt": {"equals": "push"}}},
            ]}}}),
            "`ci.evnt` is not a fact",
        ),
        (
            json!({"fields": {"task": {"fields": {"name": {"kind": "string"}}}}}),
            "`task.name` is not a fact",
        ),
    ] {
        let mut definitions = plan_and_apply();
        definitions.insert("typo".to_string(), definition(constraint, &[]));
        let error = validate(&env, &definitions).unwrap_err().to_string();
        assert!(error.contains("contexts.typo.when"), "{error}");
        assert!(error.contains(expected), "{error}");
    }
}
