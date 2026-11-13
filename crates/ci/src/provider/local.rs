use super::git::{Checkout, Since};
use super::{CIProvider, ChangedFiles, NoBase};
use crate::context::CIContext;
use crate::report::{CheckHandle, PipelineReport};
use async_trait::async_trait;
use cuenv_core::Result;
use std::collections::HashSet;
use std::path::PathBuf;

/// CI provider for runs no other provider recognises: a run on a developer's
/// machine, or a run on a CI system cuenv has no provider for.
pub struct LocalProvider {
    context: CIContext,
    base: LocalBase,
}

/// What a run without a CI provider is compared with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalBase {
    /// `HEAD`: a local run, which sees the changes not yet committed.
    Head,
    /// The commit `HEAD` and a ref both descend from, given with `--from`.
    MergeBaseWith(String),
    /// Nothing. `CI` is set, so this is a clean checkout on a CI system
    /// cuenv does not recognise: nothing is uncommitted there, and nothing
    /// says which commit the run came after.
    UnrecognisedCi,
}

impl LocalProvider {
    /// Create a `LocalProvider` that compares against a specific base reference.
    /// This will detect changes between the base ref and HEAD, plus uncommitted changes.
    #[must_use]
    pub fn with_base_ref(base_ref: String) -> Self {
        Self {
            context: local_context(Some(base_ref.clone())),
            base: LocalBase::MergeBaseWith(base_ref),
        }
    }

    /// The `--from` ref this run compares with, when one was given.
    fn base_ref(&self) -> Option<&str> {
        match &self.base {
            LocalBase::MergeBaseWith(base_ref) => Some(base_ref),
            LocalBase::Head | LocalBase::UnrecognisedCi => None,
        }
    }
}

fn local_context(base_ref: Option<String>) -> CIContext {
    CIContext {
        provider: "local".to_string(),
        event: "manual".to_string(),
        ref_name: "current".to_string(),
        base_ref,
        sha: "current".to_string(),
    }
}

/// Whether `CI` says the run is on a CI system: set, and neither empty nor a
/// spelling of "no".
fn ci_is_set(value: Option<&str>) -> bool {
    value.map(str::trim).is_some_and(|value| {
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

#[async_trait]
impl CIProvider for LocalProvider {
    /// Always available as the fallback. With `CI` set, the run is on a CI
    /// system that no provider recognised, and such a run has no base.
    fn detect() -> Option<Self> {
        let base = if ci_is_set(std::env::var("CI").ok().as_deref()) {
            LocalBase::UnrecognisedCi
        } else {
            LocalBase::Head
        };
        Some(Self {
            context: local_context(None),
            base,
        })
    }

    fn context(&self) -> &CIContext {
        &self.context
    }

    async fn changed_files(&self) -> Result<ChangedFiles> {
        if self.base == LocalBase::UnrecognisedCi {
            return Ok(ChangedFiles::NoBase(
                NoBase::new("`CI` is set and cuenv does not recognise this CI system")
                    .with_remedy("Pass --from <ref> to compare with that ref instead"),
            ));
        }

        let Some(checkout) = Checkout::discover() else {
            // Without a repository there is nothing to compare with. Saying
            // "nothing changed" would skip every task with inputs.
            return match self.base_ref() {
                Some(base_ref) => Err(cuenv_core::Error::configuration(format!(
                    "--from {base_ref}: this directory is not inside a git repository, so \
                     there is nothing to compare with"
                ))),
                None => Ok(ChangedFiles::NoBase(NoBase::new(
                    "this directory is not inside a git repository",
                ))),
            };
        };

        // Committed changes since the base ref: what is in HEAD that the
        // base ref does not have. A base ref that cannot be compared is an
        // error: without the comparison only uncommitted changes would be
        // seen, and a clean checkout would report that nothing is affected.
        let committed = self
            .base_ref()
            .map(|base_ref| {
                checkout
                    .compare(Since::MergeBaseWith(base_ref))
                    .map_err(|error| {
                        cuenv_core::Error::configuration(format!("--from {base_ref}: {error}"))
                    })
            })
            .transpose()?;

        // Always include uncommitted changes (staged + unstaged).
        let uncommitted = checkout.uncommitted().map_err(|error| {
            cuenv_core::Error::execution(format!(
                "Could not list the uncommitted changes of this checkout: {error}"
            ))
        })?;
        let changed: HashSet<PathBuf> =
            committed.into_iter().flatten().chain(uncommitted).collect();

        Ok(ChangedFiles::Compared(changed.into_iter().collect()))
    }

    async fn create_check(&self, _name: &str) -> Result<CheckHandle> {
        Ok(CheckHandle {
            id: "local".to_string(),
        })
    }

    async fn update_check(&self, _handle: &CheckHandle, _summary: &str) -> Result<()> {
        Ok(())
    }

    async fn complete_check(&self, _handle: &CheckHandle, _report: &PipelineReport) -> Result<()> {
        Ok(())
    }

    async fn upload_report(&self, _report: &PipelineReport) -> Result<Option<String>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ContextReport, PipelineStatus};
    use std::collections::HashMap;

    fn make_test_report() -> PipelineReport {
        PipelineReport {
            version: "1.0".to_string(),
            project: "test".to_string(),
            pipeline: "ci".to_string(),
            context: ContextReport {
                provider: "local".to_string(),
                event: "manual".to_string(),
                ref_name: "current".to_string(),
                base_ref: None,
                sha: "abc123".to_string(),
                changed_files: vec![],
                no_base: None,
            },
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: Some(100),
            status: PipelineStatus::Success,
            tasks: vec![],
            annotations: HashMap::new(),
        }
    }

    #[test]
    fn test_local_provider_detect() {
        let provider = LocalProvider::detect();
        assert!(provider.is_some());

        let provider = provider.unwrap();
        let ctx = provider.context();
        assert_eq!(ctx.provider, "local");
        assert_eq!(ctx.event, "manual");
        assert_eq!(ctx.ref_name, "current");
        assert!(ctx.base_ref.is_none());
    }

    #[test]
    fn test_local_provider_with_base_ref() {
        let provider = LocalProvider::with_base_ref("main".to_string());
        let ctx = provider.context();

        assert_eq!(ctx.provider, "local");
        assert_eq!(ctx.event, "manual");
        assert_eq!(ctx.base_ref, Some("main".to_string()));
    }

    #[test]
    fn test_local_provider_context_sha() {
        let provider = LocalProvider::detect().unwrap();
        let ctx = provider.context();
        assert_eq!(ctx.sha, "current");
    }

    #[tokio::test]
    async fn test_local_provider_create_check() {
        let provider = LocalProvider::detect().unwrap();
        let handle = provider.create_check("test-check").await.unwrap();
        assert_eq!(handle.id, "local");
    }

    #[tokio::test]
    async fn test_local_provider_update_check() {
        let provider = LocalProvider::detect().unwrap();
        let handle = CheckHandle {
            id: "local".to_string(),
        };
        let result = provider.update_check(&handle, "running").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_local_provider_complete_check() {
        let provider = LocalProvider::detect().unwrap();
        let handle = CheckHandle {
            id: "local".to_string(),
        };
        let report = make_test_report();
        let result = provider.complete_check(&handle, &report).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_local_provider_upload_report_returns_none() {
        let provider = LocalProvider::detect().unwrap();
        let report = make_test_report();
        let result = provider.upload_report(&report).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_local_provider_changed_files() {
        // This test is dependent on git, but should work in any repo
        let provider = LocalProvider::detect().unwrap();
        let result = provider.changed_files().await;
        // Should succeed even if there are no changes
        assert!(result.is_ok());
    }

    #[test]
    fn ci_counts_as_set_unless_it_is_empty_false_or_zero() {
        for value in ["true", "1", "yes", "TRUE", "woodpecker"] {
            assert!(ci_is_set(Some(value)), "{value:?}");
        }
        for value in ["", " ", "false", "FALSE", "False", "0"] {
            assert!(!ci_is_set(Some(value)), "{value:?}");
        }
        assert!(!ci_is_set(None));
    }

    #[tokio::test]
    async fn a_ci_system_no_provider_recognises_has_no_base() {
        let provider = temp_env::with_var("CI", Some("true"), LocalProvider::detect).unwrap();

        let changes = provider.changed_files().await.unwrap();

        let ChangedFiles::NoBase(why) = changes else {
            panic!("expected no base, got {changes:?}");
        };
        assert_eq!(
            why.to_string(),
            "No comparable base (`CI` is set and cuenv does not recognise this CI system): \
             running the whole pipeline. Pass --from <ref> to compare with that ref instead"
        );
    }

    #[test]
    fn a_local_run_is_compared_with_head() {
        // Whether it then has a base depends on the directory it runs in,
        // which `ci_no_base.rs` covers end to end.
        for value in [None, Some(""), Some("false"), Some("0")] {
            let provider = temp_env::with_var("CI", value, LocalProvider::detect).unwrap();

            assert_eq!(provider.base, LocalBase::Head, "CI={value:?}");
        }
    }

    #[test]
    fn from_gives_a_run_its_base_whatever_ci_says() {
        let provider = temp_env::with_var("CI", Some("true"), || {
            LocalProvider::with_base_ref("main".to_string())
        });

        assert_eq!(provider.base, LocalBase::MergeBaseWith("main".to_string()));
    }

    #[tokio::test]
    async fn a_base_ref_that_cannot_be_compared_is_an_error() {
        let provider =
            LocalProvider::with_base_ref("refs/heads/no-such-branch-for-cuenv".to_string());

        let error = provider
            .changed_files()
            .await
            .expect_err("a base ref that names nothing gives no comparison");

        assert!(
            error
                .to_string()
                .contains("--from refs/heads/no-such-branch-for-cuenv"),
            "{error}"
        );
    }
}
