use super::*;

#[test]
fn test_ir_version() {
    let ir = IntermediateRepresentation::new("test-pipeline");
    assert_eq!(ir.version, "1.5");
    assert_eq!(ir.pipeline.name, "test-pipeline");
    assert!(ir.runtimes.is_empty());
    assert!(ir.tasks.is_empty());
}

#[test]
fn test_purity_mode_serialization() {
    let strict = PurityMode::Strict;
    let json = serde_json::to_string(&strict).unwrap();
    assert_eq!(json, r#""strict""#);

    let warning = PurityMode::Warning;
    let json = serde_json::to_string(&warning).unwrap();
    assert_eq!(json, r#""warning""#);

    let override_mode = PurityMode::Override;
    let json = serde_json::to_string(&override_mode).unwrap();
    assert_eq!(json, r#""override""#);
}

#[test]
fn test_cache_policy_serialization() {
    let normal = CachePolicy::Normal;
    assert_eq!(serde_json::to_string(&normal).unwrap(), r#""normal""#);

    let readonly = CachePolicy::Readonly;
    assert_eq!(serde_json::to_string(&readonly).unwrap(), r#""readonly""#);

    let writeonly = CachePolicy::Writeonly;
    assert_eq!(serde_json::to_string(&writeonly).unwrap(), r#""writeonly""#);

    let disabled = CachePolicy::Disabled;
    assert_eq!(serde_json::to_string(&disabled).unwrap(), r#""disabled""#);
}

#[test]
fn test_output_type_serialization() {
    let cas = OutputType::Cas;
    assert_eq!(serde_json::to_string(&cas).unwrap(), r#""cas""#);

    let orchestrator = OutputType::Orchestrator;
    assert_eq!(
        serde_json::to_string(&orchestrator).unwrap(),
        r#""orchestrator""#
    );
}

#[test]
fn test_task_minimal() {
    let task = Task {
        id: "test-task".to_string(),
        runtime: None,
        command: vec!["echo".to_string(), "hello".to_string()],
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
    };

    let json = serde_json::to_value(&task).unwrap();
    assert_eq!(json["id"], "test-task");
    assert_eq!(json["command"], serde_json::json!(["echo", "hello"]));
    assert_eq!(json["shell"], false);
}

#[test]
fn test_task_with_deployment() {
    let task = Task {
        id: "deploy-prod".to_string(),
        runtime: None,
        command: vec!["deploy".to_string()],
        shell: false,
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        resources: None,
        concurrency_group: Some("production".to_string()),
        inputs: vec![],
        outputs: vec![],
        depends_on: vec!["build".to_string()],
        cache_policy: CachePolicy::Disabled,
        deployment: true,
        manual_approval: true,
        matrix: None,
        artifact_downloads: vec![],
        params: BTreeMap::new(),
        phase: None,
        label: None,
        priority: None,
        contributor: None,
        condition: None,
        provider_hints: None,
    };

    let json = serde_json::to_value(&task).unwrap();
    assert_eq!(json["deployment"], true);
    assert_eq!(json["manual_approval"], true);
    assert_eq!(json["cache_policy"], "disabled");
    assert_eq!(json["concurrency_group"], "production");
}

#[test]
fn test_task_with_matrix() {
    let task = Task {
        id: "build-matrix".to_string(),
        runtime: None,
        command: vec!["cargo".to_string(), "build".to_string()],
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
        matrix: Some(MatrixConfig {
            dimensions: [(
                "arch".to_string(),
                vec!["x64".to_string(), "arm64".to_string()],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        }),
        artifact_downloads: vec![],
        params: BTreeMap::new(),
        phase: None,
        label: None,
        priority: None,
        contributor: None,
        condition: None,
        provider_hints: None,
    };

    let json = serde_json::to_value(&task).unwrap();
    assert_eq!(
        json["matrix"]["dimensions"]["arch"],
        serde_json::json!(["x64", "arm64"])
    );
}

#[test]
fn test_artifact_download() {
    let artifact = ArtifactDownload {
        name: "build-${{ matrix.arch }}".to_string(),
        path: "./artifacts".to_string(),
        filter: "*stable".to_string(),
    };

    let json = serde_json::to_value(&artifact).unwrap();
    assert_eq!(json["name"], "build-${{ matrix.arch }}");
    assert_eq!(json["path"], "./artifacts");
    assert_eq!(json["filter"], "*stable");
}

#[test]
fn test_secret_config() {
    let secret = SecretConfig {
        source: "CI_API_KEY".to_string(),
        cache_key: true,
    };

    let json = serde_json::to_value(&secret).unwrap();
    assert_eq!(json["source"], "CI_API_KEY");
    assert_eq!(json["cache_key"], true);
}

#[test]
fn test_runtime() {
    let runtime = Runtime {
        id: "nix-rust".to_string(),
        flake: "github:NixOS/nixpkgs/nixos-unstable".to_string(),
        output: "devShells.x86_64-linux.default".to_string(),
        system: "x86_64-linux".to_string(),
        digest: "sha256:abc123".to_string(),
        purity: PurityMode::Strict,
    };

    let json = serde_json::to_value(&runtime).unwrap();
    assert_eq!(json["id"], "nix-rust");
    assert_eq!(json["purity"], "strict");
}

#[test]
fn test_full_ir_serialization() {
    let mut ir = IntermediateRepresentation::new("my-pipeline");
    ir.pipeline.trigger = Some(TriggerCondition {
        branches: vec!["main".to_string()],
        ..Default::default()
    });

    ir.runtimes.push(Runtime {
        id: "default".to_string(),
        flake: "github:NixOS/nixpkgs/nixos-unstable".to_string(),
        output: "devShells.x86_64-linux.default".to_string(),
        system: "x86_64-linux".to_string(),
        digest: "sha256:def456".to_string(),
        purity: PurityMode::Warning,
    });

    ir.tasks.push(Task {
        id: "build".to_string(),
        runtime: Some("default".to_string()),
        command: vec!["cargo".to_string(), "build".to_string()],
        shell: false,
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        resources: Some(ResourceRequirements {
            cpu: Some("2".to_string()),
            memory: Some("4Gi".to_string()),
            tags: vec!["rust".to_string()],
        }),
        concurrency_group: None,
        inputs: vec!["src/**/*.rs".to_string(), "Cargo.toml".to_string()],
        outputs: vec![OutputDeclaration {
            path: "target/release/binary".to_string(),
            output_type: OutputType::Cas,
        }],
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
    });

    let json = serde_json::to_string_pretty(&ir).unwrap();
    assert!(json.contains(r#""version": "1.5""#));
    assert!(json.contains(r#""name": "my-pipeline""#));
    assert!(json.contains(r#""id": "build""#));
}

// =============================================================================
// Stage Configuration Tests (v1.4)
// =============================================================================

#[test]
fn test_build_stage_serialization() {
    assert_eq!(
        serde_json::to_string(&BuildStage::Bootstrap).unwrap(),
        r#""bootstrap""#
    );
    assert_eq!(
        serde_json::to_string(&BuildStage::Setup).unwrap(),
        r#""setup""#
    );
    assert_eq!(
        serde_json::to_string(&BuildStage::Success).unwrap(),
        r#""success""#
    );
    assert_eq!(
        serde_json::to_string(&BuildStage::Failure).unwrap(),
        r#""failure""#
    );
}

// =============================================================================
// Phase Task Filtering and Sorting Tests (v1.5)
// =============================================================================

/// Helper to create a minimal task for testing
fn make_test_task(id: &str) -> Task {
    Task {
        id: id.to_string(),
        runtime: None,
        command: vec!["echo".to_string()],
        shell: false,
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
        phase: None,
        label: None,
        priority: None,
        contributor: None,
        condition: None,
        provider_hints: None,
    }
}

#[test]
fn test_phase_tasks_filters_by_phase() {
    let mut ir = IntermediateRepresentation::new("test");

    // Add regular task (no phase)
    ir.tasks.push(make_test_task("regular-task"));

    // Add bootstrap phase task
    let mut bootstrap_task = make_test_task("install-nix");
    bootstrap_task.phase = Some(BuildStage::Bootstrap);
    ir.tasks.push(bootstrap_task);

    // Add setup phase task
    let mut setup_task = make_test_task("setup-cuenv");
    setup_task.phase = Some(BuildStage::Setup);
    ir.tasks.push(setup_task);

    // Verify phase_tasks filters correctly
    let bootstrap_tasks: Vec<_> = ir.phase_tasks(BuildStage::Bootstrap).collect();
    assert_eq!(bootstrap_tasks.len(), 1);
    assert_eq!(bootstrap_tasks[0].id, "install-nix");

    let setup_tasks: Vec<_> = ir.phase_tasks(BuildStage::Setup).collect();
    assert_eq!(setup_tasks.len(), 1);
    assert_eq!(setup_tasks[0].id, "setup-cuenv");

    // Success phase should be empty
    let success_tasks: Vec<_> = ir.phase_tasks(BuildStage::Success).collect();
    assert!(success_tasks.is_empty());
}

#[test]
fn test_regular_tasks_excludes_phase_tasks() {
    let mut ir = IntermediateRepresentation::new("test");

    // Add regular tasks
    ir.tasks.push(make_test_task("build"));
    ir.tasks.push(make_test_task("test"));

    // Add phase task
    let mut phase_task = make_test_task("install-nix");
    phase_task.phase = Some(BuildStage::Bootstrap);
    ir.tasks.push(phase_task);

    // Verify regular_tasks excludes phase tasks
    let regular: Vec<_> = ir.regular_tasks().collect();
    assert_eq!(regular.len(), 2);
    assert!(regular.iter().any(|t| t.id == "build"));
    assert!(regular.iter().any(|t| t.id == "test"));
    assert!(!regular.iter().any(|t| t.id == "install-nix"));
}

#[test]
fn test_sorted_phase_tasks_orders_by_priority() {
    let mut ir = IntermediateRepresentation::new("test");

    // Add tasks with different priorities (lower = earlier)
    let mut task_high_priority = make_test_task("first");
    task_high_priority.phase = Some(BuildStage::Setup);
    task_high_priority.priority = Some(1);
    ir.tasks.push(task_high_priority);

    let mut task_low_priority = make_test_task("last");
    task_low_priority.phase = Some(BuildStage::Setup);
    task_low_priority.priority = Some(100);
    ir.tasks.push(task_low_priority);

    let mut task_medium_priority = make_test_task("middle");
    task_medium_priority.phase = Some(BuildStage::Setup);
    task_medium_priority.priority = Some(50);
    ir.tasks.push(task_medium_priority);

    // Verify sorted order
    let sorted = ir.sorted_phase_tasks(BuildStage::Setup);
    assert_eq!(sorted.len(), 3);
    assert_eq!(sorted[0].id, "first");
    assert_eq!(sorted[1].id, "middle");
    assert_eq!(sorted[2].id, "last");
}

#[test]
fn test_sorted_phase_tasks_uses_default_priority() {
    let mut ir = IntermediateRepresentation::new("test");

    // Task with explicit low priority
    let mut explicit_task = make_test_task("explicit");
    explicit_task.phase = Some(BuildStage::Setup);
    explicit_task.priority = Some(5);
    ir.tasks.push(explicit_task);

    // Task with no priority (defaults to 10)
    let mut default_task = make_test_task("default");
    default_task.phase = Some(BuildStage::Setup);
    ir.tasks.push(default_task);

    // Task with high priority (> 10)
    let mut high_task = make_test_task("high");
    high_task.phase = Some(BuildStage::Setup);
    high_task.priority = Some(20);
    ir.tasks.push(high_task);

    // Verify: explicit (5) < default (10) < high (20)
    let sorted = ir.sorted_phase_tasks(BuildStage::Setup);
    assert_eq!(sorted[0].id, "explicit");
    assert_eq!(sorted[1].id, "default");
    assert_eq!(sorted[2].id, "high");
}
