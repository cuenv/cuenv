//! File watcher for service restart-on-change.
//!
//! Uses the `notify` crate to watch file paths and trigger service restarts
//! after a debounce window.

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::debug;

/// Event emitted when watched files change.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    /// Paths that changed.
    pub paths: Vec<PathBuf>,
}

/// Watches files for changes and sends debounced events.
pub struct ServiceWatcher {
    _watcher: RecommendedWatcher,
}

impl ServiceWatcher {
    /// Start watching the given paths, sending events to the provided channel.
    ///
    /// Glob patterns in `watch_paths` are expanded relative to `project_root`.
    /// Changes are debounced by `debounce` duration.
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher cannot be initialized.
    pub fn start(
        project_root: &Path,
        watch_paths: &[String],
        ignore_patterns: &[String],
        debounce: Duration,
        event_tx: mpsc::Sender<WatchEvent>,
    ) -> crate::Result<Self> {
        let (notify_tx, notify_rx) = std_mpsc::channel();

        let mut watcher = RecommendedWatcher::new(notify_tx, Config::default()).map_err(|e| {
            crate::Error::session(format!("failed to create file watcher: {e}"))
        })?;

        // Resolve and watch paths
        let mut resolved_paths = Vec::new();
        for pattern in watch_paths {
            let full_pattern = project_root.join(pattern);
            let pattern_str = full_pattern.to_string_lossy();

            // If it's a glob pattern, watch the parent directory
            if pattern_str.contains('*') || pattern_str.contains('?') {
                // Find the deepest non-glob parent
                let mut dir = full_pattern.as_path();
                while let Some(parent) = dir.parent() {
                    let parent_str = parent.to_string_lossy();
                    if !parent_str.contains('*') && !parent_str.contains('?') {
                        if parent.exists() {
                            resolved_paths.push(parent.to_path_buf());
                        }
                        break;
                    }
                    dir = parent;
                }
            } else if full_pattern.exists() {
                resolved_paths.push(full_pattern);
            }
        }

        for path in &resolved_paths {
            if let Err(e) = watcher.watch(path, RecursiveMode::Recursive) {
                debug!("Failed to watch {}: {}", path.display(), e);
            }
        }

        // Build ignore matcher
        let ignore_globs: Vec<glob::Pattern> = ignore_patterns
            .iter()
            .filter_map(|p| glob::Pattern::new(p).ok())
            .collect();

        // Spawn debounce task
        tokio::spawn(async move {
            let mut pending: Vec<PathBuf> = Vec::new();
            let mut debounce_timer: Option<tokio::time::Instant> = None;

            loop {
                // Check for new notify events (non-blocking)
                while let Ok(event) = notify_rx.try_recv() {
                    if let Ok(event) = event {
                        for path in event.paths {
                            // Check ignore patterns
                            let path_str = path.to_string_lossy();
                            let ignored = ignore_globs
                                .iter()
                                .any(|g| g.matches(&path_str));

                            if !ignored && !pending.contains(&path) {
                                pending.push(path);
                            }
                        }
                        if !pending.is_empty() {
                            debounce_timer = Some(tokio::time::Instant::now() + debounce);
                        }
                    }
                }

                // Check if debounce timer has fired
                if let Some(deadline) = debounce_timer
                    && tokio::time::Instant::now() >= deadline
                    && !pending.is_empty()
                {
                    let paths = std::mem::take(&mut pending);
                    debounce_timer = None;
                    if event_tx.send(WatchEvent { paths }).await.is_err() {
                        // Receiver dropped — stop watching
                        break;
                    }
                }

                // Small sleep to avoid busy-looping
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        Ok(Self { _watcher: watcher })
    }
}
