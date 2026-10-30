//! Object keys for tenant state, and the grammar every key segment obeys.
//!
//! ```text
//! [<root>/]<module>/_<package>/<project>/<env>/state.json
//! [<root>/]<module>/_<package>/<project>/<env>/history/<serial:020>.json
//! [<root>/]<module>/_<package>/<project>/_migration.json
//! [<root>/]_locks/<module>/_<package>/<project>/<env>/<lock id>.json
//! ```
//!
//! Every segment is checked before a key is built, and every character a
//! segment may hold is RFC 3986 unreserved, so keys go into request paths
//! verbatim. CUE module path elements start with a letter or digit, so the
//! first element starting with `_` is where the module path ends, and the
//! first element of a module path contains a dot, so `_locks` and `root`
//! (whose elements may not contain a dot) can never be mistaken for one.

use crate::error::{InfrastructureError, Result, strip_control_characters};
use crate::tenant::TenantKey;

/// The environment segment of the identity without `--env`.
pub const DEFAULT_ENVIRONMENT: &str = "_default";

/// The object holding a tenant's state.
pub const STATE_OBJECT: &str = "state.json";

/// The folder of a tenant's history snapshots.
pub const HISTORY_FOLDER: &str = "history";

/// The per-project migration manifest.
pub const MIGRATION_MANIFEST: &str = "_migration.json";

/// The top-level folder of lock markers.
pub const LOCKS_FOLDER: &str = "_locks";

/// Digits of a history serial in its key, so keys sort as numbers.
const SERIAL_DIGITS: usize = 20;

/// A tenant's place in a bucket: the root, the module path, the package, the
/// project and the environment, each validated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantPath {
    root: Option<String>,
    module_path: String,
    package: String,
    project: String,
    environment: Option<String>,
}

impl TenantPath {
    /// The path of `tenant` in `package` under `root`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] when any segment breaks
    /// the key grammar.
    pub fn new(root: Option<&str>, package: &str, tenant: &TenantKey) -> Result<Self> {
        if let Some(root) = root {
            validate_root(root)?;
        }
        validate_module_path(tenant.module_path())?;
        validate_package(package)?;
        validate_project(tenant.project())?;
        if let Some(environment) = tenant.environment() {
            validate_environment(environment)?;
        }
        Ok(Self {
            root: root.map(ToString::to_string),
            module_path: tenant.module_path().to_string(),
            package: package.to_string(),
            project: tenant.project().to_string(),
            environment: tenant.environment().map(ToString::to_string),
        })
    }

    /// The tenant this path holds.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] when the parts do not
    /// form a tenant (never for a path built by [`Self::new`] or parsed by
    /// [`parse_state_key`]).
    pub fn tenant(&self) -> Result<TenantKey> {
        match &self.environment {
            Some(environment) => {
                TenantKey::with_environment(&self.module_path, &self.project, environment)
            }
            None => TenantKey::try_new(&self.module_path, &self.project),
        }
    }

    /// The CUE package.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// The module path.
    #[must_use]
    pub fn module_path(&self) -> &str {
        &self.module_path
    }

    /// The project name.
    #[must_use]
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The environment, `None` without `--env`.
    #[must_use]
    pub fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }

    /// The same tenant in another package, project or environment.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] when a new segment
    /// breaks the key grammar.
    pub fn with(&self, package: &str, tenant: &TenantKey) -> Result<Self> {
        Self::new(self.root.as_deref(), package, tenant)
    }

    fn root_prefix(&self) -> String {
        self.root
            .as_ref()
            .map_or_else(String::new, |root| format!("{root}/"))
    }

    fn environment_segment(&self) -> &str {
        self.environment.as_deref().unwrap_or(DEFAULT_ENVIRONMENT)
    }

    /// `[<root>/]<module>/_<package>/<project>/`.
    #[must_use]
    pub fn project_prefix(&self) -> String {
        format!(
            "{}{}/_{}/{}/",
            self.root_prefix(),
            self.module_path,
            self.package,
            self.project
        )
    }

    /// `[<root>/]<module>/_<package>/<project>/<env>/`.
    #[must_use]
    pub fn tenant_prefix(&self) -> String {
        format!("{}{}/", self.project_prefix(), self.environment_segment())
    }

    /// The key of the tenant's `state.json`.
    #[must_use]
    pub fn state_key(&self) -> String {
        format!("{}{STATE_OBJECT}", self.tenant_prefix())
    }

    /// The prefix of the tenant's history snapshots.
    #[must_use]
    pub fn history_prefix(&self) -> String {
        format!("{}{HISTORY_FOLDER}/", self.tenant_prefix())
    }

    /// The key of the history snapshot at `serial`, zero-padded so keys sort
    /// as numbers.
    #[must_use]
    pub fn history_key(&self, serial: u64) -> String {
        format!(
            "{}{serial:0width$}.json",
            self.history_prefix(),
            width = SERIAL_DIGITS
        )
    }

    /// The key of the project's migration manifest.
    #[must_use]
    pub fn migration_manifest_key(&self) -> String {
        format!("{}{MIGRATION_MANIFEST}", self.project_prefix())
    }

    /// The prefix of the tenant's lock markers.
    #[must_use]
    pub fn marker_prefix(&self) -> String {
        format!(
            "{}{LOCKS_FOLDER}/{}/_{}/{}/{}/",
            self.root_prefix(),
            self.module_path,
            self.package,
            self.project,
            self.environment_segment()
        )
    }

    /// The key of the marker of the lock `lock_identifier`.
    #[must_use]
    pub fn marker_key(&self, lock_identifier: &str) -> String {
        format!("{}{lock_identifier}.json", self.marker_prefix())
    }

    /// The key of a transient capability-probe object inside the tenant
    /// prefix.
    #[must_use]
    pub fn probe_key(&self, nonce: &str) -> String {
        format!("{}.probe-{nonce}", self.tenant_prefix())
    }
}

/// The prefix of every tenant of `module_path` in every package:
/// `[<root>/]<module>/_` (the trailing `_` keeps nested modules out).
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] for an invalid root or
/// module path.
pub fn module_prefix(root: Option<&str>, module_path: &str) -> Result<String> {
    if let Some(root) = root {
        validate_root(root)?;
    }
    validate_module_path(module_path)?;
    Ok(format!(
        "{}{module_path}/_",
        root.map_or_else(String::new, |root| format!("{root}/"))
    ))
}

/// The prefix of every lock marker, of one module or of the whole bucket
/// root.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] for an invalid root or
/// module path.
pub fn markers_prefix(root: Option<&str>, module_path: Option<&str>) -> Result<String> {
    if let Some(root) = root {
        validate_root(root)?;
    }
    let root = root.map_or_else(String::new, |root| format!("{root}/"));
    match module_path {
        Some(module_path) => {
            validate_module_path(module_path)?;
            Ok(format!("{root}{LOCKS_FOLDER}/{module_path}/_"))
        }
        None => Ok(format!("{root}{LOCKS_FOLDER}/")),
    }
}

/// Split the part of a key after the root into the tenant path segments and
/// what follows: `<module…>/_<package>/<project>/<env>/<rest…>`.
fn split_tenant(root: Option<&str>, relative: &str) -> Option<(TenantPath, String)> {
    let elements: Vec<&str> = relative.split('/').collect();
    let package_index = elements
        .iter()
        .position(|element| element.starts_with('_'))?;
    if package_index == 0 || elements.len() < package_index + 4 {
        return None;
    }
    let module_path = elements[..package_index].join("/");
    let package = elements[package_index].strip_prefix('_')?;
    let project = elements[package_index + 1];
    let environment = match elements[package_index + 2] {
        DEFAULT_ENVIRONMENT => None,
        other => Some(other),
    };
    let rest = elements[package_index + 3..].join("/");
    let tenant = match environment {
        Some(environment) => TenantKey::with_environment(&module_path, project, environment),
        None => TenantKey::try_new(&module_path, project),
    }
    .ok()?;
    // `TenantKey` strips a trailing `@vN`; a key holding one is not ours.
    if tenant.module_path() != module_path {
        return None;
    }
    let path = TenantPath::new(root, package, &tenant).ok()?;
    Some((path, rest))
}

fn strip_root<'key>(root: Option<&str>, key: &'key str) -> Option<&'key str> {
    match root {
        Some(root) => key.strip_prefix(root)?.strip_prefix('/'),
        None => Some(key),
    }
}

/// Parse the key of a tenant's `state.json`. Returns `None` for any key that
/// is not exactly one: keys read from a listing are untrusted.
#[must_use]
pub fn parse_state_key(root: Option<&str>, key: &str) -> Option<TenantPath> {
    let relative = strip_root(root, key)?;
    if relative.starts_with(&format!("{LOCKS_FOLDER}/")) {
        return None;
    }
    let (path, rest) = split_tenant(root, relative)?;
    (rest == STATE_OBJECT).then_some(path)
}

/// Parse the key of a lock marker into the tenant path and the lock
/// identifier. Returns `None` for anything else.
#[must_use]
pub fn parse_marker_key(root: Option<&str>, key: &str) -> Option<(TenantPath, String)> {
    let relative = strip_root(root, key)?.strip_prefix(&format!("{LOCKS_FOLDER}/"))?;
    let (path, rest) = split_tenant(root, relative)?;
    let identifier = rest.strip_suffix(".json")?;
    let lock = crate::state::StateLock {
        lock_identifier: identifier.to_string(),
    };
    lock.validate().ok()?;
    Some((path, identifier.to_string()))
}

/// Parse the serial out of a history snapshot key under `prefix`.
#[must_use]
pub fn parse_history_serial(prefix: &str, key: &str) -> Option<u64> {
    let name = key.strip_prefix(prefix)?.strip_suffix(".json")?;
    (name.len() == SERIAL_DIGITS && name.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| name.parse().ok())
        .flatten()
}

fn invalid(segment: &str, value: &str, rule: &str) -> InfrastructureError {
    InfrastructureError::configuration(format!(
        "object-storage state: {segment} '{}' {rule}",
        strip_control_characters(value)
    ))
}

/// A CUE module path: lowercase ASCII letters, digits, `.`, `_` and `-`,
/// elements separated by `/`, each starting with a letter or digit, the
/// first containing a dot, no empty element and no `..`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] for anything else.
pub fn validate_module_path(module_path: &str) -> Result<()> {
    const RULE: &str = "is not a valid CUE module path (lowercase letters, digits, '.', '_' and \
                        '-'; each element starts with a letter or digit; the first contains a \
                        dot)";
    let elements: Vec<&str> = module_path.split('/').collect();
    let valid = !module_path.is_empty()
        && !module_path.contains("..")
        && elements.first().is_some_and(|first| first.contains('.'))
        && elements.iter().all(|element| {
            element
                .bytes()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
                && element.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
                })
        });
    if valid {
        Ok(())
    } else {
        Err(invalid("module path", module_path, RULE))
    }
}

/// A bucket root: elements of lowercase letters, digits, `_` and `-`, each
/// starting with a letter or digit. No dots, so the first dotted element of
/// a key is always where the module path starts.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] for anything else.
pub fn validate_root(root: &str) -> Result<()> {
    let valid = !root.is_empty()
        && root.split('/').all(|element| {
            element
                .bytes()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
                && element.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
                })
        });
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "root",
            root,
            "must be '/'-separated elements of lowercase letters, digits, '_' and '-', each \
             starting with a letter or digit (no dots)",
        ))
    }
}

/// A CUE package name: a letter, then letters, digits and `_`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] for anything else.
pub fn validate_package(package: &str) -> Result<()> {
    let valid = package
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "package",
            package,
            "must start with a letter and hold only letters, digits and '_'",
        ))
    }
}

/// A project name: a letter or digit, then letters, digits, `.`, `_` and
/// `-`, never containing `..`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] for anything else.
pub fn validate_project(project: &str) -> Result<()> {
    let valid = project
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && !project.contains("..")
        && project
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "project name",
            project,
            "must start with a letter or digit, hold only letters, digits, '.', '_' and '-', \
             and never contain '..'",
        ))
    }
}

/// An environment name (`#InfrastructureName`): a letter, then letters,
/// digits, `_` and `-`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] for anything else.
pub fn validate_environment(environment: &str) -> Result<()> {
    let valid = environment
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && environment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "environment",
            environment,
            "must start with a letter and hold only letters, digits, '_' and '-'",
        ))
    }
}

/// Refuse a set of environment names where two differ only by case: a
/// store on a case-folding filesystem (MinIO on macOS, for one) would merge
/// their keys.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] naming the two.
pub fn validate_environment_case<'name>(names: impl IntoIterator<Item = &'name str>) -> Result<()> {
    let mut seen: std::collections::BTreeMap<String, &str> = std::collections::BTreeMap::new();
    for name in names {
        if let Some(previous) = seen.insert(name.to_ascii_lowercase(), name)
            && previous != name
        {
            return Err(InfrastructureError::configuration(format!(
                "environments '{}' and '{}' differ only by case; object-storage state keys \
                 would collide on a case-insensitive store, so rename one",
                strip_control_characters(previous),
                strip_control_characters(name)
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(environment: Option<&str>) -> TenantKey {
        match environment {
            Some(environment) => {
                TenantKey::with_environment("github.com/acme/infra", "web", environment).unwrap()
            }
            None => TenantKey::try_new("github.com/acme/infra", "web").unwrap(),
        }
    }

    #[test]
    fn keys_follow_the_layout() {
        let path = TenantPath::new(None, "cuenv", &tenant(None)).unwrap();
        assert_eq!(
            path.state_key(),
            "github.com/acme/infra/_cuenv/web/_default/state.json"
        );
        assert_eq!(
            path.history_key(42),
            "github.com/acme/infra/_cuenv/web/_default/history/00000000000000000042.json"
        );
        assert_eq!(
            path.migration_manifest_key(),
            "github.com/acme/infra/_cuenv/web/_migration.json"
        );
        assert_eq!(
            path.marker_key("abc-1"),
            "_locks/github.com/acme/infra/_cuenv/web/_default/abc-1.json"
        );
        let rooted = TenantPath::new(Some("state/v1"), "ops", &tenant(Some("Prod"))).unwrap();
        assert_eq!(
            rooted.state_key(),
            "state/v1/github.com/acme/infra/_ops/web/Prod/state.json"
        );
        assert_eq!(
            rooted.marker_key("x"),
            "state/v1/_locks/github.com/acme/infra/_ops/web/Prod/x.json"
        );
        assert_eq!(
            module_prefix(Some("state"), "github.com/acme/infra").unwrap(),
            "state/github.com/acme/infra/_"
        );
        assert_eq!(markers_prefix(None, None).unwrap(), "_locks/");
    }

    #[test]
    fn keys_parse_back_and_strangers_are_refused() {
        for (root, package, environment) in
            [(None, "cuenv", None), (Some("state"), "ops", Some("Prod"))]
        {
            let path = TenantPath::new(root, package, &tenant(environment)).unwrap();
            assert_eq!(parse_state_key(root, &path.state_key()), Some(path.clone()));
            assert_eq!(
                parse_marker_key(root, &path.marker_key("lock-1")),
                Some((path.clone(), "lock-1".to_string()))
            );
            assert_eq!(
                parse_history_serial(&path.history_prefix(), &path.history_key(7)),
                Some(7)
            );
        }
        for key in [
            "github.com/acme/infra/_cuenv/web/_default/other.json",
            "github.com/acme/infra/_cuenv/web/state.json",
            "_locks/github.com/acme/infra/_cuenv/web/_default/state.json",
            "github.com/acme/infra/_cuenv/../_default/state.json",
            "acme/infra/_cuenv/web/_default/state.json",
            "github.com/Acme/infra/_cuenv/web/_default/state.json",
            "github.com/acme/infra@v1/_cuenv/web/_default/state.json",
            "github.com/acme/infra/_cuenv/web/_bad/state.json",
        ] {
            assert_eq!(parse_state_key(None, key), None, "{key}");
        }
        assert_eq!(
            parse_marker_key(
                None,
                "_locks/github.com/acme/infra/_cuenv/web/_default/a b.json"
            ),
            None
        );
        assert_eq!(
            parse_history_serial("p/", "p/42.json"),
            None,
            "unpadded serials are not ours"
        );
    }

    #[test]
    fn a_nested_module_cannot_produce_another_modules_key() {
        let outer = TenantPath::new(
            None,
            "cuenv",
            &TenantKey::try_new("github.com/acme/infra", "web").unwrap(),
        )
        .unwrap();
        let nested = TenantPath::new(
            None,
            "cuenv",
            &TenantKey::try_new("github.com/acme/infra/web", "web").unwrap(),
        )
        .unwrap();
        assert_ne!(outer.state_key(), nested.state_key());
        assert!(
            !nested
                .state_key()
                .starts_with(&module_prefix(None, "github.com/acme/infra").unwrap())
        );
    }

    #[test]
    fn segments_outside_the_grammar_are_refused() {
        for module in [
            "",
            "acme/infra",
            "github.com/Acme/infra",
            "github.com//infra",
            "github.com/_infra",
            "github.com/.infra",
            "github.com/a..b",
            "github.com/a b",
            "github.com/ä",
        ] {
            assert!(validate_module_path(module).is_err(), "{module}");
        }
        validate_module_path("github.com/acme/infra-2/a_b.c").unwrap();
        for root in ["", "a.b", "_x", "a//b", "A", "x/y.z"] {
            assert!(validate_root(root).is_err(), "{root}");
        }
        validate_root("state/v1_x").unwrap();
        for package in ["", "_x", "1x", "x-y", "x.y", "é"] {
            assert!(validate_package(package).is_err(), "{package}");
        }
        validate_package("Cuenv_2").unwrap();
        for project in ["", ".", "..", ".x", "a..b", "-x", "a/b", "a b"] {
            assert!(validate_project(project).is_err(), "{project}");
        }
        validate_project("my.app-2_x").unwrap();
        for environment in ["", "_default", "1x", "a.b", "a/b", ".."] {
            assert!(validate_environment(environment).is_err(), "{environment}");
        }
        validate_environment("Prod-2_x").unwrap();
        assert!(TenantPath::new(None, "cuenv", &tenant(Some("_default"))).is_err());
    }

    #[test]
    fn environments_differing_only_by_case_are_refused() {
        validate_environment_case(["Dev", "Prod"]).unwrap();
        let error = validate_environment_case(["Dev", "Prod", "dev"]).unwrap_err();
        assert!(error.to_string().contains("'Dev' and 'dev'"), "{error}");
    }
}
