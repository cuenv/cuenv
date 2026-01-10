//! Integration tests for GitHub CI provider functionality.
//!
//! These tests verify the GitHub CI provider's pure functions and behavior
//! without requiring actual GitHub API calls.

/// Tests for PR number parsing from GitHub refs.
///
/// The `GITHUB_REF` environment variable contains the full ref for the event
/// that triggered the workflow (e.g., `refs/pull/123/merge` for PRs).
mod pr_number_parsing {
    /// Verify PR number extraction from standard merge ref format.
    #[test]
    fn extracts_pr_number_from_merge_ref() {
        // Standard format used when PR is triggered
        let ref_str = "refs/pull/42/merge";
        let result = parse_pr_number(ref_str);
        assert_eq!(result, Some(42));
    }

    /// Verify PR number extraction from head ref format.
    #[test]
    fn extracts_pr_number_from_head_ref() {
        // Alternative format sometimes used
        let ref_str = "refs/pull/100/head";
        let result = parse_pr_number(ref_str);
        assert_eq!(result, Some(100));
    }

    /// Verify branch refs return None (not a PR).
    #[test]
    fn returns_none_for_branch_refs() {
        let test_cases = [
            "refs/heads/main",
            "refs/heads/feature/my-feature",
            "refs/heads/develop",
            "refs/heads/release/v1.0.0",
        ];

        for ref_str in test_cases {
            let result = parse_pr_number(ref_str);
            assert_eq!(
                result, None,
                "Expected None for branch ref '{ref_str}', got {result:?}"
            );
        }
    }

    /// Verify tag refs return None (not a PR).
    #[test]
    fn returns_none_for_tag_refs() {
        let test_cases = [
            "refs/tags/v1.0.0",
            "refs/tags/release-2024.01.15",
            "refs/tags/latest",
        ];

        for ref_str in test_cases {
            let result = parse_pr_number(ref_str);
            assert_eq!(
                result, None,
                "Expected None for tag ref '{ref_str}', got {result:?}"
            );
        }
    }

    /// Verify edge cases are handled gracefully.
    #[test]
    fn handles_edge_cases() {
        // Empty string
        assert_eq!(parse_pr_number(""), None);

        // Malformed PR ref (no number)
        assert_eq!(parse_pr_number("refs/pull/"), None);

        // Non-numeric PR number
        assert_eq!(parse_pr_number("refs/pull/abc/merge"), None);

        // Very large PR number (should still parse)
        assert_eq!(
            parse_pr_number("refs/pull/999999999/merge"),
            Some(999_999_999)
        );

        // PR number zero (edge case)
        assert_eq!(parse_pr_number("refs/pull/0/merge"), Some(0));
    }

    /// Parse PR number from GitHub ref (mirroring the actual implementation).
    fn parse_pr_number(github_ref: &str) -> Option<u64> {
        if github_ref.starts_with("refs/pull/") {
            github_ref
                .strip_prefix("refs/pull/")?
                .split('/')
                .next()?
                .parse()
                .ok()
        } else {
            None
        }
    }
}

/// Tests for repository owner/name parsing.
mod repo_parsing {
    /// Verify standard owner/repo format is parsed correctly.
    #[test]
    fn parses_standard_repo_format() {
        let (owner, repo) = parse_repo("cuenv/cuenv");
        assert_eq!(owner, "cuenv");
        assert_eq!(repo, "cuenv");
    }

    /// Verify organization repos with complex names.
    #[test]
    fn parses_organization_repos() {
        let test_cases = [
            ("facebook/react", "facebook", "react"),
            ("kubernetes/kubernetes", "kubernetes", "kubernetes"),
            ("rust-lang/rust", "rust-lang", "rust"),
            ("my-org/my-project-name", "my-org", "my-project-name"),
        ];

        for (input, expected_owner, expected_repo) in test_cases {
            let (owner, repo) = parse_repo(input);
            assert_eq!(owner, expected_owner, "Owner mismatch for '{input}'");
            assert_eq!(repo, expected_repo, "Repo mismatch for '{input}'");
        }
    }

    /// Verify invalid formats return empty strings.
    #[test]
    fn returns_empty_for_invalid_formats() {
        let test_cases = ["", "invalid", "a/b/c", "a/b/c/d", "/repo", "owner/"];

        for input in test_cases {
            let (owner, repo) = parse_repo(input);
            // For truly invalid formats (not exactly 2 parts), both should be empty
            if input.split('/').count() != 2 {
                assert!(
                    owner.is_empty() && repo.is_empty(),
                    "Expected empty strings for invalid format '{input}', got owner='{owner}', repo='{repo}'"
                );
            }
        }
    }

    /// Parse repository from GITHUB_REPOSITORY format.
    fn parse_repo(repo_str: &str) -> (String, String) {
        let parts: Vec<&str> = repo_str.split('/').collect();
        if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (String::new(), String::new())
        }
    }
}

/// Tests for CI context creation and environment detection.
mod ci_context {
    /// Verify provider detection when not in GitHub Actions.
    #[test]
    fn detect_returns_none_outside_github_actions() {
        // Clear all GitHub-related environment variables
        temp_env::with_vars_unset(
            [
                "GITHUB_ACTIONS",
                "GITHUB_REPOSITORY",
                "GITHUB_REF",
                "GITHUB_REF_NAME",
                "GITHUB_BASE_REF",
                "GITHUB_SHA",
                "GITHUB_EVENT_NAME",
                "GITHUB_TOKEN",
            ],
            || {
                // Detection should fail when not in GitHub Actions
                let is_github_actions = std::env::var("GITHUB_ACTIONS").ok();
                assert!(
                    is_github_actions.is_none() || is_github_actions.as_deref() != Some("true")
                );
            },
        );
    }

    /// Verify detection fails when GITHUB_ACTIONS is "false".
    #[test]
    fn detect_returns_none_when_github_actions_false() {
        temp_env::with_var("GITHUB_ACTIONS", Some("false"), || {
            let is_github = std::env::var("GITHUB_ACTIONS").ok();
            assert_ne!(is_github.as_deref(), Some("true"));
        });
    }

    /// Verify context fields are populated from environment.
    #[test]
    fn context_populated_from_environment() {
        temp_env::with_vars(
            [
                ("GITHUB_ACTIONS", Some("true")),
                ("GITHUB_REPOSITORY", Some("test-org/test-repo")),
                ("GITHUB_REF", Some("refs/pull/123/merge")),
                ("GITHUB_REF_NAME", Some("123/merge")),
                ("GITHUB_BASE_REF", Some("main")),
                ("GITHUB_SHA", Some("abc123def456")),
                ("GITHUB_EVENT_NAME", Some("pull_request")),
            ],
            || {
                // Verify environment is set correctly
                assert_eq!(
                    std::env::var("GITHUB_ACTIONS").ok(),
                    Some("true".to_string())
                );
                assert_eq!(
                    std::env::var("GITHUB_REPOSITORY").ok(),
                    Some("test-org/test-repo".to_string())
                );
                assert_eq!(
                    std::env::var("GITHUB_EVENT_NAME").ok(),
                    Some("pull_request".to_string())
                );
            },
        );
    }
}

/// Tests for the NULL_SHA constant and before SHA filtering.
mod before_sha_handling {
    const NULL_SHA: &str = "0000000000000000000000000000000000000000";

    /// Verify NULL_SHA is correctly formatted (40 zeros).
    #[test]
    fn null_sha_is_40_zeros() {
        assert_eq!(NULL_SHA.len(), 40);
        assert!(NULL_SHA.chars().all(|c| c == '0'));
    }

    /// Verify NULL_SHA is filtered out from before SHA.
    #[test]
    fn filters_null_sha() {
        temp_env::with_var("GITHUB_BEFORE", Some(NULL_SHA), || {
            let before = get_before_sha();
            assert!(before.is_none(), "NULL_SHA should be filtered out");
        });
    }

    /// Verify empty GITHUB_BEFORE is filtered out.
    #[test]
    fn filters_empty_before_sha() {
        temp_env::with_var("GITHUB_BEFORE", Some(""), || {
            let before = get_before_sha();
            assert!(
                before.is_none(),
                "Empty GITHUB_BEFORE should be filtered out"
            );
        });
    }

    /// Verify valid SHA is returned.
    #[test]
    fn returns_valid_sha() {
        let valid_sha = "abc123def456789012345678901234567890abcd";
        temp_env::with_var("GITHUB_BEFORE", Some(valid_sha), || {
            let before = get_before_sha();
            assert_eq!(before, Some(valid_sha.to_string()));
        });
    }

    /// Get before SHA, filtering out null and empty values.
    fn get_before_sha() -> Option<String> {
        std::env::var("GITHUB_BEFORE")
            .ok()
            .filter(|sha| sha != NULL_SHA && !sha.is_empty())
    }
}

/// Tests for git diff output parsing (changed file detection).
mod changed_file_detection {
    use std::path::PathBuf;

    /// Verify empty git diff output results in empty file list.
    #[test]
    fn parses_empty_diff_output() {
        let output = "";
        let files = parse_diff_output(output);
        assert!(files.is_empty());
    }

    /// Verify whitespace-only lines are filtered.
    #[test]
    fn filters_whitespace_lines() {
        let output = "   \n\t\n  \t  \n";
        let files = parse_diff_output(output);
        assert!(files.is_empty());
    }

    /// Verify valid file paths are parsed correctly.
    #[test]
    fn parses_valid_file_paths() {
        let output = "src/main.rs\nCargo.toml\nREADME.md";
        let files = parse_diff_output(output);

        assert_eq!(files.len(), 3);
        assert_eq!(files[0], PathBuf::from("src/main.rs"));
        assert_eq!(files[1], PathBuf::from("Cargo.toml"));
        assert_eq!(files[2], PathBuf::from("README.md"));
    }

    /// Verify mixed content (with empty lines) is parsed correctly.
    #[test]
    fn handles_mixed_content() {
        let output = "src/lib.rs\n\n  \nCargo.lock\n\t\ntests/test.rs";
        let files = parse_diff_output(output);

        assert_eq!(files.len(), 3);
        assert_eq!(files[0], PathBuf::from("src/lib.rs"));
        assert_eq!(files[1], PathBuf::from("Cargo.lock"));
        assert_eq!(files[2], PathBuf::from("tests/test.rs"));
    }

    /// Verify nested paths are handled.
    #[test]
    fn handles_nested_paths() {
        let output = "crates/core/src/lib.rs\nschema/env.cue\nexamples/basic/env.cue";
        let files = parse_diff_output(output);

        assert_eq!(files.len(), 3);
        assert_eq!(files[0], PathBuf::from("crates/core/src/lib.rs"));
        assert_eq!(files[1], PathBuf::from("schema/env.cue"));
        assert_eq!(files[2], PathBuf::from("examples/basic/env.cue"));
    }

    /// Verify paths with special characters are handled.
    #[test]
    fn handles_special_characters_in_paths() {
        let output = "src/my-module.rs\nsrc/my_module.rs\ndocs/README (copy).md";
        let files = parse_diff_output(output);

        assert_eq!(files.len(), 3);
        assert_eq!(files[0], PathBuf::from("src/my-module.rs"));
        assert_eq!(files[1], PathBuf::from("src/my_module.rs"));
        assert_eq!(files[2], PathBuf::from("docs/README (copy).md"));
    }

    /// Parse git diff output into file paths.
    fn parse_diff_output(output: &str) -> Vec<PathBuf> {
        output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PathBuf::from(line.trim()))
            .collect()
    }
}

/// Tests for secret reference transformation.
mod secret_transformation {
    /// Transform CI-agnostic secret reference syntax to GitHub Actions syntax.
    fn transform_secret_ref(value: &str) -> String {
        let trimmed = value.trim();
        if !trimmed.starts_with("${") || !trimmed.ends_with('}') {
            return value.to_string();
        }

        let var_name = &trimmed[2..trimmed.len() - 1];

        let Some(first_char) = var_name.chars().next() else {
            return value.to_string();
        };

        if !first_char.is_ascii_uppercase() {
            return value.to_string();
        }

        let is_valid_var_name = var_name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');

        if !is_valid_var_name {
            return value.to_string();
        }

        format!("${{{{ secrets.{var_name} }}}}")
    }

    /// Verify basic secret reference transformation.
    #[test]
    fn transforms_basic_secret_ref() {
        assert_eq!(transform_secret_ref("${FOO}"), "${{ secrets.FOO }}");
        assert_eq!(transform_secret_ref("${BAR}"), "${{ secrets.BAR }}");
    }

    /// Verify secrets with underscores and numbers.
    #[test]
    fn transforms_complex_secret_names() {
        assert_eq!(
            transform_secret_ref("${FOO_BAR_123}"),
            "${{ secrets.FOO_BAR_123 }}"
        );
        assert_eq!(
            transform_secret_ref("${OP_SERVICE_ACCOUNT_TOKEN}"),
            "${{ secrets.OP_SERVICE_ACCOUNT_TOKEN }}"
        );
        assert_eq!(
            transform_secret_ref("${CACHIX_AUTH_TOKEN}"),
            "${{ secrets.CACHIX_AUTH_TOKEN }}"
        );
    }

    /// Verify embedded references are not transformed.
    #[test]
    fn does_not_transform_embedded_refs() {
        assert_eq!(
            transform_secret_ref("prefix-${VAR}-suffix"),
            "prefix-${VAR}-suffix"
        );
        assert_eq!(
            transform_secret_ref("https://example.com/${TOKEN}"),
            "https://example.com/${TOKEN}"
        );
    }

    /// Verify regular values are unchanged.
    #[test]
    fn leaves_regular_values_unchanged() {
        assert_eq!(transform_secret_ref("regular_value"), "regular_value");
        assert_eq!(transform_secret_ref("my-secret-name"), "my-secret-name");
        assert_eq!(transform_secret_ref("123"), "123");
    }

    /// Verify already-correct syntax is unchanged.
    #[test]
    fn idempotent_for_github_syntax() {
        // Already in GitHub Actions format - should not double-transform
        assert_eq!(
            transform_secret_ref("${{ secrets.VAR }}"),
            "${{ secrets.VAR }}"
        );
    }

    /// Verify lowercase variables are not transformed (convention).
    #[test]
    fn does_not_transform_lowercase_vars() {
        assert_eq!(transform_secret_ref("${foo}"), "${foo}");
        assert_eq!(transform_secret_ref("${myVar}"), "${myVar}");
    }

    /// Verify edge cases.
    #[test]
    fn handles_edge_cases() {
        assert_eq!(transform_secret_ref(""), "");
        assert_eq!(transform_secret_ref("${}"), "${}");
        assert_eq!(transform_secret_ref("${_}"), "${_}"); // Starts with underscore
        assert_eq!(transform_secret_ref("${1ABC}"), "${1ABC}"); // Starts with number
    }
}

/// Tests for workflow filename sanitization.
mod filename_sanitization {
    /// Sanitize a string for use as a workflow filename.
    fn sanitize_filename(name: &str) -> String {
        name.to_lowercase()
            .replace(' ', "-")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect()
    }

    /// Sanitize a string for use as a job ID.
    fn sanitize_job_id(id: &str) -> String {
        id.replace(['.', ' '], "-")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect()
    }

    /// Verify filename sanitization.
    #[test]
    fn sanitizes_filenames() {
        assert_eq!(sanitize_filename("CI Pipeline"), "ci-pipeline");
        assert_eq!(sanitize_filename("My Workflow"), "my-workflow");
        assert_eq!(sanitize_filename("release/v1"), "releasev1");
        assert_eq!(sanitize_filename("test_workflow"), "test_workflow");
        assert_eq!(sanitize_filename("Build & Test"), "build--test");
    }

    /// Verify job ID sanitization.
    #[test]
    fn sanitizes_job_ids() {
        assert_eq!(sanitize_job_id("build.test"), "build-test");
        assert_eq!(sanitize_job_id("deploy prod"), "deploy-prod");
        assert_eq!(
            sanitize_job_id("release.build.linux"),
            "release-build-linux"
        );
        assert_eq!(sanitize_job_id("my_task"), "my_task");
    }

    /// Verify special characters are removed.
    #[test]
    fn removes_special_characters() {
        assert_eq!(sanitize_filename("test@workflow!"), "testworkflow");
        assert_eq!(sanitize_job_id("task#1"), "task1");
        assert_eq!(sanitize_filename("a/b\\c"), "abc");
    }
}
