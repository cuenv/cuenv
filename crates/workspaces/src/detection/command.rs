use crate::core::types::PackageManager;

/// Returns the effective command name from a shell-like command string.
///
/// This skips common environment variable prefixes such as `FOO=bar cargo test`
/// and `env -i FOO=bar bun run build`, while respecting shell quoting rules.
#[must_use]
pub fn command_name(command: &str) -> Option<String> {
    let tokens = shlex::split(command)?;
    let mut parsing_env_options = false;

    for token in tokens {
        if token == "env" {
            parsing_env_options = true;
            continue;
        }

        if parsing_env_options {
            if token == "--" {
                parsing_env_options = false;
                continue;
            }

            if token.starts_with('-') {
                continue;
            }
        }

        if is_env_assignment(&token) {
            continue;
        }

        return Some(token);
    }

    None
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };

    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }

    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// Infers the package manager from a command string.
///
/// Maps common command names to their corresponding package managers.
///
/// # Examples
///
/// ```
/// use cuenv_workspaces::detection::detect_from_command;
/// use cuenv_workspaces::PackageManager;
///
/// assert_eq!(detect_from_command("cargo"), Some(PackageManager::Cargo));
/// assert_eq!(detect_from_command("npm"), Some(PackageManager::Npm));
/// assert_eq!(detect_from_command("bun"), Some(PackageManager::Bun));
/// assert_eq!(detect_from_command("pnpm"), Some(PackageManager::Pnpm));
/// assert_eq!(detect_from_command("node"), Some(PackageManager::Npm));
/// assert_eq!(detect_from_command("unknown"), None);
/// ```
pub fn detect_from_command(command: &str) -> Option<PackageManager> {
    let cmd = command_name(command)?;

    match cmd.as_str() {
        "cargo" => Some(PackageManager::Cargo),
        "npm" | "npx" | "node" => Some(PackageManager::Npm),
        "bun" | "bunx" => Some(PackageManager::Bun),
        "pnpm" => Some(PackageManager::Pnpm),
        "deno" => Some(PackageManager::Deno),
        "yarn" => {
            tracing::warn!(
                "'yarn' command detected; defaulting to YarnClassic. For accurate version detection, use lockfile analysis via detect_yarn_version()."
            );
            Some(PackageManager::YarnClassic)
        }
        _ => None,
    }
}
