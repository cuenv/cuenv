//! Read-only cache diagnostics commands (`cuenv cache status | stats | verify`).

use crate::commands::task::{cache as task_cache, remote_cache};
use cuenv_core::{Error, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Output payload for `cuenv cache status`.
#[derive(Debug, Serialize)]
pub struct CacheStatusOutput {
    /// Resolved local cache directory.
    pub local_root: PathBuf,
    /// Whether the local cache root currently exists on disk.
    pub local_exists: bool,
    /// Remote cache configuration and live connectivity probe.
    pub remote: remote_cache::RemoteCacheStatus,
}

fn resolve_project_dir(path: &str) -> Result<PathBuf> {
    let raw = Path::new(path);
    if raw.is_absolute() {
        return Ok(raw.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|e| Error::io("get current directory", e))?;
    Ok(cwd.join(raw))
}

fn load_project_cache_config(
    project_dir: &Path,
    package: &str,
) -> Option<cuenv_core::manifest::Cache> {
    let target_path = project_dir.canonicalize().ok()?;
    let module_root = crate::commands::env_file::find_cue_module_root(&target_path)?;
    let options = cuengine::ModuleEvalOptions {
        recursive: false,
        target_dir: Some(target_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    let raw = cuengine::evaluate_module(&module_root, package, Some(&options)).ok()?;
    let rel_path = crate::commands::relative_path_from_root(&module_root, &target_path);
    let key = rel_path.to_string_lossy();
    let instance_json = raw
        .instances
        .get(key.as_ref())
        .or_else(|| raw.instances.get("."))?;
    let cache_value = instance_json.get("cache")?;
    serde_json::from_value(cache_value.clone()).ok()
}

/// Execute `cuenv cache status`.
///
/// # Errors
///
/// Returns an error if the working directory cannot be resolved or JSON output
/// cannot be serialized.
pub async fn execute_status(path: &str, package: &str, format: &str) -> Result<()> {
    let project_dir = resolve_project_dir(path)?;
    let local_root = task_cache::resolve_cache_root(&project_dir);
    let cache_config = load_project_cache_config(&project_dir, package);
    let remote = remote_cache::probe_status(cache_config.as_ref()).await;

    let output = CacheStatusOutput {
        local_exists: local_root.exists(),
        local_root,
        remote,
    };

    if format == "json" {
        let json = serde_json::to_string_pretty(&output)
            .map_err(|e| Error::configuration(format!("serialize cache status: {e}")))?;
        cuenv_events::println_redacted(&json);
        return Ok(());
    }

    let mut lines = vec![format!(
        "Local cache: {} ({})",
        output.local_root.display(),
        if output.local_exists {
            "present"
        } else {
            "not initialized"
        }
    )];
    if output.remote.configured {
        let endpoint = output.remote.endpoint.as_deref().unwrap_or("");
        let protocol = output.remote.protocol.as_deref().unwrap_or("grpc");
        lines.push(format!(
            "Remote cache: {endpoint} (protocol: {protocol}, state: {})",
            output.remote.state
        ));
        if let Some(instance) = &output.remote.instance
            && !instance.is_empty()
        {
            lines.push(format!("  instance: {instance}"));
        }
        lines.push(format!(
            "  upload: {}",
            if output.remote.upload_enabled {
                "enabled"
            } else if output.remote.upload_requested {
                "disabled (requested, awaiting strict confinement)"
            } else {
                "read-only"
            }
        ));
        if let Some(detail) = &output.remote.detail {
            lines.push(format!("  detail: {detail}"));
        }
    } else {
        lines.push("Remote cache: disabled (local-only)".to_string());
    }

    cuenv_events::println_redacted(&lines.join("\n"));
    Ok(())
}

/// Execute `cuenv cache stats`.
///
/// # Errors
///
/// Returns an error if the cache root cannot be inspected.
pub fn execute_stats(path: &str, format: &str) -> Result<()> {
    let project_dir = resolve_project_dir(path)?;
    let local_root = task_cache::resolve_cache_root(&project_dir);
    let stats = cuenv_cas::maintenance::stats(&local_root)
        .map_err(|e| Error::configuration(format!("inspect cache stats: {e}")))?;

    if format == "json" {
        let json = serde_json::to_string_pretty(&stats)
            .map_err(|e| Error::configuration(format!("serialize cache stats: {e}")))?;
        cuenv_events::println_redacted(&json);
        return Ok(());
    }

    let text = format!(
        "Cache root: {}\n  CAS blobs:      {} ({})\n  Action entries: {} ({})\n  Temp staging:   {}\n  Exec roots:     {}\n  Total:          {}",
        stats.root.display(),
        stats.cas_blobs,
        format_bytes(stats.cas_bytes),
        stats.action_entries,
        format_bytes(stats.action_bytes),
        format_bytes(stats.temp_bytes),
        format_bytes(stats.exec_bytes),
        format_bytes(stats.total_bytes()),
    );
    cuenv_events::println_redacted(&text);
    Ok(())
}

/// Verification mode for `cuenv cache verify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Inspect the store without modifying any files.
    ReadOnly,
    /// Delete corrupt CAS blobs and invalid/dangling ActionCache entries.
    Repair,
}

/// Arguments for `cuenv cache gc`.
#[derive(Debug, Clone)]
pub struct CacheGcArgs {
    /// Path to the CUE module or project directory.
    pub path: String,
    /// CUE package containing the cache configuration.
    pub package: String,
    /// Output format (`text` or `json`).
    pub format: String,
    /// Optional target maximum cache size string (e.g. `2G`, `500M`).
    pub max_size: Option<String>,
    /// Optional maximum entry age string (e.g. `7d`, `24h`).
    pub max_age: Option<String>,
    /// Preview deletions without removing files.
    pub dry_run: bool,
}

/// Arguments for `cuenv cache prune`.
#[derive(Debug, Clone)]
pub struct CachePruneArgs {
    /// Path to the CUE module or project directory.
    pub path: String,
    /// Output format (`text` or `json`).
    pub format: String,
    /// Preview deletions without removing files.
    pub dry_run: bool,
}

/// Execute `cuenv cache verify`.
///
/// # Errors
///
/// Returns an error if the store cannot be read or (in read-only mode) if any
/// corrupt blobs or invalid/dangling action entries are found.
pub async fn execute_verify(path: &str, format: &str, mode: VerifyMode) -> Result<()> {
    let project_dir = resolve_project_dir(path)?;
    let local_root = task_cache::resolve_cache_root(&project_dir);

    if mode == VerifyMode::Repair {
        let repair = cuenv_cas::maintenance::verify_and_repair(&local_root)
            .await
            .map_err(|e| Error::configuration(format!("repair local cache: {e}")))?;

        if format == "json" {
            let json = serde_json::to_string_pretty(&repair)
                .map_err(|e| Error::configuration(format!("serialize repair report: {e}")))?;
            cuenv_events::println_redacted(&json);
        } else {
            let text = format!(
                "Verified and repaired cache at {}: checked {} CAS blob(s), {} action entry(ies); removed {} corrupt blob(s), {} invalid action entry(ies)",
                repair.verify.root.display(),
                repair.verify.cas_blobs_checked,
                repair.verify.action_entries_checked,
                repair.blobs_removed,
                repair.actions_removed,
            );
            cuenv_events::println_redacted(&text);
        }
        return Ok(());
    }

    let report = cuenv_cas::maintenance::verify(&local_root)
        .await
        .map_err(|e| Error::configuration(format!("verify local cache: {e}")))?;

    if format == "json" {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|e| Error::configuration(format!("serialize verify report: {e}")))?;
        cuenv_events::println_redacted(&json);
    } else {
        let mut lines = vec![format!(
            "Verified cache at {}: {} CAS blob(s), {} action entry(ies)",
            report.root.display(),
            report.cas_blobs_checked,
            report.action_entries_checked
        )];
        if report.is_ok() {
            lines.push("Status: OK".to_string());
        } else {
            lines.push(format!(
                "Status: FAILED ({} corrupt blob(s), {} invalid action entry(ies))",
                report.corrupt_blobs.len(),
                report.invalid_actions.len()
            ));
            for blob in &report.corrupt_blobs {
                lines.push(format!(
                    "  corrupt blob {}: {}",
                    blob.expected_hash, blob.reason
                ));
            }
            for action in &report.invalid_actions {
                lines.push(format!(
                    "  invalid action {}: {}",
                    action.action_hash, action.reason
                ));
            }
        }
        cuenv_events::println_redacted(&lines.join("\n"));
    }

    if !report.is_ok() {
        return Err(Error::configuration(format!(
            "cache verification failed: {} corrupt blob(s), {} invalid action entry(ies)",
            report.corrupt_blobs.len(),
            report.invalid_actions.len()
        )));
    }
    Ok(())
}

/// Execute `cuenv cache gc`.
///
/// # Errors
///
/// Returns an error if size/duration flags are invalid or GC fails.
pub async fn execute_gc(args: CacheGcArgs) -> Result<()> {
    let project_dir = resolve_project_dir(&args.path)?;
    let local_root = task_cache::resolve_cache_root(&project_dir);
    let max_size = args.max_size.or_else(|| {
        load_project_cache_config(&project_dir, &args.package)?
            .local?
            .max_size
    });
    let max_bytes = max_size.as_deref().map(parse_byte_size).transpose()?;
    let max_age = args.max_age.as_deref().map(parse_duration).transpose()?;

    let report = cuenv_cas::maintenance::gc(
        &local_root,
        cuenv_cas::GcOptions {
            max_bytes,
            max_age,
            grace_window: cuenv_cas::DEFAULT_GC_GRACE_WINDOW,
            dry_run: args.dry_run,
        },
    )
    .await
    .map_err(|e| Error::configuration(format!("garbage-collect local cache: {e}")))?;

    render_gc_report(&report, &args.format)
}

/// Execute `cuenv cache prune`.
///
/// # Errors
///
/// Returns an error if the local cache cannot be pruned.
pub async fn execute_prune(args: CachePruneArgs) -> Result<()> {
    let project_dir = resolve_project_dir(&args.path)?;
    let local_root = task_cache::resolve_cache_root(&project_dir);

    let report = cuenv_cas::maintenance::prune(
        &local_root,
        cuenv_cas::PruneOptions {
            grace_window: cuenv_cas::DEFAULT_GC_GRACE_WINDOW,
            dry_run: args.dry_run,
        },
    )
    .await
    .map_err(|e| Error::configuration(format!("prune local cache: {e}")))?;

    render_gc_report(&report, &args.format)
}

fn render_gc_report(report: &cuenv_cas::GcReport, format: &str) -> Result<()> {
    if format == "json" {
        let json = serde_json::to_string_pretty(report)
            .map_err(|e| Error::configuration(format!("serialize gc report: {e}")))?;
        cuenv_events::println_redacted(&json);
        return Ok(());
    }

    let prefix = if report.dry_run { "[dry-run] " } else { "" };
    let text = format!(
        "{prefix}Cache cleanup at {}:\n  Action entries removed: {} ({})\n  CAS blobs removed:      {} ({})\n  Temp entries removed:   {} ({})\n  Total freed:            {}\n  Protected (grace):      {} blob(s), {} action entry(ies)",
        report.root.display(),
        report.actions_removed,
        format_bytes(report.actions_bytes_freed),
        report.blobs_removed,
        format_bytes(report.blobs_bytes_freed),
        report.temp_entries_removed,
        format_bytes(report.temp_bytes_freed),
        format_bytes(report.total_bytes_freed()),
        report.protected_recent_blobs,
        report.protected_recent_actions,
    );
    cuenv_events::println_redacted(&text);
    Ok(())
}

fn parse_byte_size(raw: &str) -> Result<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::configuration("byte size must not be empty"));
    }
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (num_str, unit_str) = trimmed.split_at(split);
    let value: u64 = num_str
        .parse()
        .map_err(|_| Error::configuration(format!("invalid byte size '{raw}'")))?;
    let multiplier: u64 = match unit_str.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        "t" | "tb" | "tib" => 1024 * 1024 * 1024 * 1024,
        _ => {
            return Err(Error::configuration(format!(
                "unsupported byte size unit in '{raw}' (expected B, K, M, G, or T)"
            )));
        }
    };
    value
        .checked_mul(multiplier)
        .ok_or_else(|| Error::configuration(format!("byte size '{raw}' overflows u64")))
}

fn parse_duration(raw: &str) -> Result<std::time::Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::configuration("duration must not be empty"));
    }
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (num_str, unit_str) = trimmed.split_at(split);
    let value: u64 = num_str
        .parse()
        .map_err(|_| Error::configuration(format!("invalid duration '{raw}'")))?;
    let seconds: u64 = match unit_str.trim().to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" => value,
        "m" | "min" | "mins" => value.saturating_mul(60),
        "h" | "hr" | "hrs" => value.saturating_mul(60 * 60),
        "d" | "day" | "days" => value.saturating_mul(24 * 60 * 60),
        "w" | "week" | "weeks" => value.saturating_mul(7 * 24 * 60 * 60),
        _ => {
            return Err(Error::configuration(format!(
                "unsupported duration unit in '{raw}' (expected s, m, h, d, or w)"
            )));
        }
    };
    Ok(std::time::Duration::from_secs(seconds))
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{}.{:02} GiB", bytes / GIB, (bytes % GIB) * 100 / GIB)
    } else if bytes >= MIB {
        format!("{}.{:02} MiB", bytes / MIB, (bytes % MIB) * 100 / MIB)
    } else if bytes >= KIB {
        format!("{}.{:02} KiB", bytes / KIB, (bytes % KIB) * 100 / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_byte_sizes_and_durations() {
        assert_eq!(parse_byte_size("512").unwrap(), 512);
        assert_eq!(parse_byte_size("2K").unwrap(), 2048);
        assert_eq!(parse_byte_size("500M").unwrap(), 500 * 1024 * 1024);
        assert_eq!(parse_byte_size("2GiB").unwrap(), 2 * 1024 * 1024 * 1024);
        assert!(parse_byte_size("10X").is_err());

        assert_eq!(parse_duration("30s").unwrap().as_secs(), 30);
        assert_eq!(parse_duration("15m").unwrap().as_secs(), 900);
        assert_eq!(parse_duration("24h").unwrap().as_secs(), 86_400);
        assert_eq!(parse_duration("7d").unwrap().as_secs(), 7 * 86_400);
        assert!(parse_duration("7").is_err());
    }
}
