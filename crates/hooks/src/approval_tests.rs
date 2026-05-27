use super::*;
use tempfile::TempDir;

fn make_hook(command: &str, args: &[&str]) -> Hook {
    Hook {
        order: 100,
        propagate: false,
        command: command.to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        dir: None,
        inputs: vec![],
        source: None,
    }
}

#[tokio::test]
async fn test_approval_manager_operations() {
    let temp_dir = TempDir::new().unwrap();
    let approval_file = temp_dir.path().join("approvals.json");
    let mut manager = ApprovalManager::new(approval_file);

    let directory = Path::new("/test/directory");
    let config_hash = "test_hash_123".to_string();

    // Initially not approved
    assert!(!manager.is_approved(directory, &config_hash).unwrap());

    // Approve configuration
    manager
        .approve_config(
            directory,
            config_hash.clone(),
            Some("Test approval".to_string()),
        )
        .await
        .unwrap();

    // Should now be approved
    assert!(manager.is_approved(directory, &config_hash).unwrap());

    // Different hash should not be approved
    assert!(!manager.is_approved(directory, "different_hash").unwrap());

    // Test persistence
    let mut manager2 = ApprovalManager::new(manager.approval_file.clone());
    manager2.load_approvals().await.unwrap();
    assert!(manager2.is_approved(directory, &config_hash).unwrap());

    // Revoke approval
    let revoked = manager2.revoke_approval(directory).await.unwrap();
    assert!(revoked);
    assert!(!manager2.is_approved(directory, &config_hash).unwrap());
}

#[test]
fn test_approval_hash_consistency() {
    // Same hooks should produce same hash
    let mut hooks_map = HashMap::new();
    hooks_map.insert("setup".to_string(), make_hook("echo", &["hello"]));
    let hooks = Hooks {
        on_enter: Some(hooks_map.clone()),
        on_exit: None,
        pre_push: None,
    };

    let hash1 = compute_approval_hash(Some(&hooks));
    let hash2 = compute_approval_hash(Some(&hooks));
    assert_eq!(hash1, hash2, "Same hooks should produce same hash");

    // Different hooks should produce different hash
    let mut hooks_map2 = HashMap::new();
    hooks_map2.insert("setup".to_string(), make_hook("echo", &["world"]));
    let hooks2 = Hooks {
        on_enter: Some(hooks_map2),
        on_exit: None,
        pre_push: None,
    };

    let hash3 = compute_approval_hash(Some(&hooks2));
    assert_ne!(
        hash1, hash3,
        "Different hooks should produce different hash"
    );
}

#[test]
fn test_approval_hash_no_hooks() {
    // Configs without hooks should produce consistent hash
    let hash1 = compute_approval_hash(None);
    let hash2 = compute_approval_hash(None);
    assert_eq!(hash1, hash2, "No hooks should produce consistent hash");

    // Empty hooks should be same as no hooks
    let empty_hooks = Hooks {
        on_enter: None,
        on_exit: None,
        pre_push: None,
    };
    let hash3 = compute_approval_hash(Some(&empty_hooks));
    assert_eq!(hash1, hash3, "Empty hooks should be same as no hooks");
}

#[test]
fn test_config_summary() {
    let mut on_enter = HashMap::new();
    on_enter.insert("npm".to_string(), make_hook("npm", &["install"]));
    on_enter.insert(
        "docker".to_string(),
        make_hook("docker-compose", &["up", "-d"]),
    );

    let mut on_exit = HashMap::new();
    on_exit.insert("docker".to_string(), make_hook("docker-compose", &["down"]));

    let hooks = Hooks {
        on_enter: Some(on_enter),
        on_exit: Some(on_exit),
        pre_push: None,
    };

    let summary = ConfigSummary::from_hooks(Some(&hooks));
    assert!(summary.has_hooks);
    assert_eq!(summary.hook_count, 3);

    let description = summary.description();
    assert!(description.contains("3 hooks"));
}

#[test]
fn test_approval_status() {
    let mut manager = ApprovalManager::new(PathBuf::from("/tmp/test"));
    let directory = Path::new("/test/dir");
    let hooks = Hooks {
        on_enter: None,
        on_exit: None,
        pre_push: None,
    };

    let status = check_approval_status_core(&manager, directory, Some(&hooks)).unwrap();
    assert!(matches!(status, ApprovalStatus::NotApproved { .. }));

    // Add an approval with a different hash
    let different_hash = "different_hash".to_string();
    manager.approvals.insert(
        compute_directory_key(directory),
        ApprovalRecord {
            directory_path: directory.to_path_buf(),
            config_hash: different_hash,
            approved_at: Utc::now(),
            expires_at: None,
            note: None,
        },
    );

    let status = check_approval_status_core(&manager, directory, Some(&hooks)).unwrap();
    assert!(matches!(status, ApprovalStatus::RequiresApproval { .. }));

    // Add approval with correct hash
    let correct_hash = compute_approval_hash(Some(&hooks));
    manager.approvals.insert(
        compute_directory_key(directory),
        ApprovalRecord {
            directory_path: directory.to_path_buf(),
            config_hash: correct_hash,
            approved_at: Utc::now(),
            expires_at: None,
            note: None,
        },
    );

    let status = check_approval_status_core(&manager, directory, Some(&hooks)).unwrap();
    assert!(matches!(status, ApprovalStatus::Approved));
}

#[test]
fn test_path_validation() {
    // Test valid paths
    assert!(validate_path_structure(Path::new("/home/user/test")).is_ok());
    assert!(validate_path_structure(Path::new("./relative/path")).is_ok());
    assert!(validate_path_structure(Path::new("file.txt")).is_ok());

    // Test paths with null bytes (should fail)
    let path_with_null = PathBuf::from("/test\0/path");
    assert!(validate_path_structure(&path_with_null).is_err());

    // Test paths with multiple parent directory traversals (should fail)
    assert!(validate_path_structure(Path::new("../../../etc/passwd")).is_err());
    assert!(validate_path_structure(Path::new("..\\..\\..\\windows\\system32")).is_err());

    // Test URL-encoded traversals (should fail)
    assert!(validate_path_structure(Path::new("/test/%2e%2e/passwd")).is_err());

    // Test semicolon injection (should fail)
    assert!(validate_path_structure(Path::new("..;/etc/passwd")).is_err());
}

#[test]
fn test_validate_and_canonicalize_path() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");
    std::fs::write(&test_file, "test").unwrap();

    // Test existing file canonicalization
    let result = validate_and_canonicalize_path(&test_file).unwrap();
    assert!(result.is_absolute());
    assert!(result.exists());

    // Test non-existent file in existing directory
    let new_file = temp_dir.path().join("new_file.txt");
    let result = validate_and_canonicalize_path(&new_file).unwrap();
    assert!(result.ends_with("new_file.txt"));

    // Test validation with parent directory that exists
    let nested_new = temp_dir.path().join("subdir/newfile.txt");
    let result = validate_and_canonicalize_path(&nested_new);
    assert!(result.is_ok()); // Should succeed even though parent doesn't exist yet
}

#[tokio::test]
async fn test_approval_file_corruption_recovery() {
    let temp_dir = TempDir::new().unwrap();
    let approval_file = temp_dir.path().join("approvals.json");

    // Write corrupted JSON to the approval file
    std::fs::write(&approval_file, "{invalid json}").unwrap();

    let mut manager = ApprovalManager::new(approval_file.clone());

    // Loading should fail due to corrupted JSON
    let result = manager.load_approvals().await;
    assert!(
        result.is_err(),
        "Expected error when loading corrupted JSON"
    );

    // Manager should still be usable with empty approvals
    assert_eq!(manager.approvals.len(), 0);

    // Should be able to save new approvals
    let directory = Path::new("/test/dir");
    manager
        .approve_config(directory, "test_hash".to_string(), None)
        .await
        .unwrap();

    // New manager should be able to load the fixed file
    let mut manager2 = ApprovalManager::new(approval_file);
    manager2.load_approvals().await.unwrap();
    assert_eq!(manager2.approvals.len(), 1);
}

#[tokio::test]
async fn test_approval_expiration() {
    let temp_dir = TempDir::new().unwrap();
    let approval_file = temp_dir.path().join("approvals.json");
    let mut manager = ApprovalManager::new(approval_file);

    let directory = Path::new("/test/expire");
    let config_hash = "expire_hash".to_string();

    // Add an expired approval
    let expired_approval = ApprovalRecord {
        directory_path: directory.to_path_buf(),
        config_hash: config_hash.clone(),
        approved_at: Utc::now() - chrono::Duration::hours(2),
        expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        note: Some("Expired approval".to_string()),
    };

    manager
        .approvals
        .insert(compute_directory_key(directory), expired_approval);

    // Should not be approved due to expiration
    assert!(!manager.is_approved(directory, &config_hash).unwrap());

    // Cleanup should remove expired approval
    let removed = manager.cleanup_expired().await.unwrap();
    assert_eq!(removed, 1);
    assert_eq!(manager.approvals.len(), 0);
}

#[test]
fn test_is_ci_with_ci_env_var() {
    // Test with CI=true
    temp_env::with_var("CI", Some("true"), || {
        assert!(is_ci());
    });

    // Test with CI=1
    temp_env::with_var("CI", Some("1"), || {
        assert!(is_ci());
    });

    // Test with CI=yes (any non-empty, non-false value)
    temp_env::with_var("CI", Some("yes"), || {
        assert!(is_ci());
    });

    // Test with CI=false (should NOT be detected as CI)
    temp_env::with_var("CI", Some("false"), || {
        // Clear other CI vars to isolate the test
        temp_env::with_vars_unset(
            vec![
                "GITHUB_ACTIONS",
                "GITLAB_CI",
                "BUILDKITE",
                "JENKINS_URL",
                "CIRCLECI",
                "TRAVIS",
                "BITBUCKET_PIPELINES",
                "AZURE_PIPELINES",
                "TF_BUILD",
                "DRONE",
                "TEAMCITY_VERSION",
            ],
            || {
                assert!(!is_ci());
            },
        );
    });

    // Test with CI=0 (should NOT be detected as CI)
    temp_env::with_var("CI", Some("0"), || {
        temp_env::with_vars_unset(
            vec![
                "GITHUB_ACTIONS",
                "GITLAB_CI",
                "BUILDKITE",
                "JENKINS_URL",
                "CIRCLECI",
                "TRAVIS",
                "BITBUCKET_PIPELINES",
                "AZURE_PIPELINES",
                "TF_BUILD",
                "DRONE",
                "TEAMCITY_VERSION",
            ],
            || {
                assert!(!is_ci());
            },
        );
    });
}

#[test]
fn test_is_ci_with_provider_specific_vars() {
    // Test GitHub Actions
    temp_env::with_var_unset("CI", || {
        temp_env::with_var("GITHUB_ACTIONS", Some("true"), || {
            assert!(is_ci());
        });
    });

    // Test GitLab CI
    temp_env::with_var_unset("CI", || {
        temp_env::with_var("GITLAB_CI", Some("true"), || {
            assert!(is_ci());
        });
    });

    // Test Buildkite
    temp_env::with_var_unset("CI", || {
        temp_env::with_var("BUILDKITE", Some("true"), || {
            assert!(is_ci());
        });
    });

    // Test Jenkins
    temp_env::with_var_unset("CI", || {
        temp_env::with_var("JENKINS_URL", Some("http://jenkins.example.com"), || {
            assert!(is_ci());
        });
    });
}

#[test]
fn test_is_ci_not_detected() {
    // Clear all CI-related environment variables
    temp_env::with_vars_unset(
        vec![
            "CI",
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "BUILDKITE",
            "JENKINS_URL",
            "CIRCLECI",
            "TRAVIS",
            "BITBUCKET_PIPELINES",
            "AZURE_PIPELINES",
            "TF_BUILD",
            "DRONE",
            "TEAMCITY_VERSION",
        ],
        || {
            assert!(!is_ci());
        },
    );
}

#[test]
fn test_approval_status_auto_approved_in_ci() {
    let manager = ApprovalManager::new(PathBuf::from("/tmp/test"));
    let directory = Path::new("/test/ci_dir");

    // Create hooks that would normally require approval
    let mut hooks_map = HashMap::new();
    hooks_map.insert("setup".to_string(), make_hook("echo", &["hello"]));

    let hooks = Hooks {
        on_enter: Some(hooks_map),
        on_exit: None,
        pre_push: None,
    };

    // In CI environment, should be auto-approved
    temp_env::with_var("CI", Some("true"), || {
        let status = check_approval_status(&manager, directory, Some(&hooks)).unwrap();
        assert!(
            matches!(status, ApprovalStatus::Approved),
            "Hooks should be auto-approved in CI"
        );
    });

    // Outside CI environment, should require approval
    temp_env::with_vars_unset(
        vec![
            "CI",
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "BUILDKITE",
            "JENKINS_URL",
            "CIRCLECI",
            "TRAVIS",
            "BITBUCKET_PIPELINES",
            "AZURE_PIPELINES",
            "TF_BUILD",
            "DRONE",
            "TEAMCITY_VERSION",
        ],
        || {
            let status = check_approval_status(&manager, directory, Some(&hooks)).unwrap();
            assert!(
                matches!(status, ApprovalStatus::NotApproved { .. }),
                "Hooks should require approval outside CI"
            );
        },
    );
}
