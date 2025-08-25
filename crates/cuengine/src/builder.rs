//! Builder pattern for configuring CUE evaluation

use crate::cache::EvaluationCache;
use crate::retry::RetryConfig;
use cuenv_core::{Limits, Result};
use std::path::Path;
use std::time::Duration;

/// Configuration builder for CUE evaluation
pub struct CueEvaluatorBuilder {
    limits: Limits,
    retry_config: Option<RetryConfig>,
    cache_capacity: Option<usize>,
    cache_ttl: Duration,
}

impl Default for CueEvaluatorBuilder {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            retry_config: Some(RetryConfig::default()),
            cache_capacity: Some(100),
            cache_ttl: Duration::from_secs(300), // 5 minutes
        }
    }
}

impl CueEvaluatorBuilder {
    /// Creates a new builder with default configuration
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum path length
    #[must_use]
    pub fn max_path_length(mut self, length: usize) -> Self {
        self.limits.max_path_length = length;
        self
    }

    /// Set maximum package name length
    #[must_use]
    pub fn max_package_name_length(mut self, length: usize) -> Self {
        self.limits.max_package_name_length = length;
        self
    }

    /// Set maximum output size
    #[must_use]
    pub fn max_output_size(mut self, size: usize) -> Self {
        self.limits.max_output_size = size;
        self
    }

    /// Configure retry behavior
    #[must_use]
    pub fn retry_config(mut self, config: RetryConfig) -> Self {
        self.retry_config = Some(config);
        self
    }

    /// Disable retries
    #[must_use]
    pub fn no_retry(mut self) -> Self {
        self.retry_config = None;
        self
    }

    /// Set cache capacity (0 to disable)
    #[must_use]
    pub fn cache_capacity(mut self, capacity: usize) -> Self {
        self.cache_capacity = if capacity == 0 { None } else { Some(capacity) };
        self
    }

    /// Set cache TTL
    #[must_use]
    pub fn cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Build the evaluator
    ///
    /// # Errors
    ///
    /// Returns an error if the cache cannot be created
    pub fn build(self) -> Result<CueEvaluator> {
        let cache = match self.cache_capacity {
            Some(capacity) => Some(EvaluationCache::new(capacity, self.cache_ttl)?),
            None => None,
        };

        Ok(CueEvaluator {
            limits: self.limits,
            retry_config: self.retry_config,
            cache,
        })
    }
}

/// CUE evaluator with configuration
pub struct CueEvaluator {
    limits: Limits,
    retry_config: Option<RetryConfig>,
    cache: Option<EvaluationCache>,
}

impl CueEvaluator {
    /// Create a new builder
    #[must_use]
    pub fn builder() -> CueEvaluatorBuilder {
        CueEvaluatorBuilder::new()
    }

    /// Evaluate a CUE package
    ///
    /// # Errors
    ///
    /// Returns an error if the path or package name validation fails,
    /// or if the CUE evaluation fails
    pub fn evaluate(&self, dir_path: &Path, package_name: &str) -> Result<String> {
        // Check cache first
        if let Some(ref cache) = self.cache {
            if let Some(cached) = cache.get(dir_path, package_name) {
                tracing::debug!("Cache hit for {}:{}", dir_path.display(), package_name);
                return Ok(cached);
            }
        }

        // Validate inputs
        crate::validation::validate_path(dir_path, &self.limits)?;
        crate::validation::validate_package_name(package_name, &self.limits)?;

        // Perform evaluation with retry if configured
        let result = if let Some(ref retry_config) = self.retry_config {
            crate::retry::with_retry(retry_config, || {
                self.evaluate_internal(dir_path, package_name)
            })?
        } else {
            self.evaluate_internal(dir_path, package_name)?
        };

        // Validate output size
        crate::validation::validate_output(&result, &self.limits)?;

        // Cache the result
        if let Some(ref cache) = self.cache {
            cache.insert(dir_path, package_name, result.clone());
        }

        Ok(result)
    }

    fn evaluate_internal(&self, dir_path: &Path, package_name: &str) -> Result<String> {
        let _ = self; // Will be used when refactored
        // Call the existing evaluate_cue_package function
        // This would be refactored to be a private implementation detail
        crate::evaluate_cue_package(dir_path, package_name)
    }

    /// Clear the cache
    pub fn clear_cache(&self) {
        if let Some(ref cache) = self.cache {
            cache.clear();
        }
    }
}
