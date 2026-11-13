//! How a declared path pattern is read.
//!
//! A task's inputs are read in several places: the walker that hashes them,
//! the cache key, affected detection, and the derivation of CI trigger
//! paths. They must agree. A pattern the walker hashes and affected
//! detection never matches makes CI skip a task whose inputs changed, so
//! every reader normalises a pattern with [`normalize`] and decides between
//! a glob and a path with [`is_glob`], and nothing else.

/// The one spelling of `raw` that every reader works with.
///
/// Surrounding whitespace is trimmed. Segments that name nothing are
/// dropped: a leading `./`, an inner `/./`, a doubled `/`, a trailing `/.`
/// and a trailing `/`. `..` segments and a leading `/` are kept, because
/// they change what the pattern means and are for the reader to refuse.
///
/// The empty string, which is also what `.` and `./` become, means the
/// directory patterns are relative to.
///
/// ```
/// use cuenv_vcs::pattern::normalize;
///
/// assert_eq!(normalize(" ./src//lib/./mod.rs "), "src/lib/mod.rs");
/// assert_eq!(normalize("src/."), "src");
/// assert_eq!(normalize("."), "");
/// assert_eq!(normalize("../shared/**"), "../shared/**");
/// assert_eq!(normalize("/etc/hosts"), "/etc/hosts");
/// ```
#[must_use]
pub fn normalize(raw: &str) -> String {
    let trimmed = raw.trim();
    let segments: Vec<&str> = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    let joined = segments.join("/");
    if trimmed.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Whether a normalised pattern is matched as a glob.
///
/// Anything else is a path: a file, or a directory that stands for every
/// file beneath it.
///
/// ```
/// use cuenv_vcs::pattern::is_glob;
///
/// assert!(is_glob("src/**/*.rs"));
/// assert!(is_glob("src/{a,b}.txt"));
/// assert!(is_glob("file?.txt"));
/// assert!(is_glob("[ab].txt"));
/// assert!(!is_glob("src/main.rs"));
/// ```
#[must_use]
pub fn is_glob(pattern: &str) -> bool {
    pattern.contains(['*', '{', '?', '['])
}

/// The directory every match of a normalised glob lies beneath: its leading
/// segments up to the first one that holds a wildcard.
///
/// Empty when the first segment is already a wildcard, meaning the glob can
/// match anywhere. For a path that is not a glob, the path itself.
///
/// ```
/// use cuenv_vcs::pattern::literal_prefix;
///
/// assert_eq!(literal_prefix("src/**/*.ts"), "src");
/// assert_eq!(literal_prefix("foo/bar/*.rs"), "foo/bar");
/// assert_eq!(literal_prefix("**/*.ts"), "");
/// assert_eq!(literal_prefix("dist/app.js"), "dist/app.js");
/// ```
#[must_use]
pub fn literal_prefix(pattern: &str) -> String {
    pattern
        .split('/')
        .take_while(|segment| !is_glob(segment))
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_that_name_nothing_are_dropped() {
        for (raw, normal) in [
            ("src", "src"),
            ("./src", "src"),
            ("src/./lib", "src/lib"),
            ("src//lib", "src/lib"),
            ("src/.", "src"),
            ("src/", "src"),
            ("././src/.//lib/", "src/lib"),
            ("./**/*.rs", "**/*.rs"),
        ] {
            assert_eq!(normalize(raw), normal, "{raw:?}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_and_inner_whitespace_kept() {
        assert_eq!(normalize("  src/** "), "src/**");
        assert_eq!(normalize("src/**\u{a0}"), "src/**");
        assert_eq!(normalize("\tbun.lock\n"), "bun.lock");
        assert_eq!(normalize("my file.txt"), "my file.txt");
    }

    #[test]
    fn the_directory_itself_is_the_empty_pattern() {
        for raw in ["", " ", ".", "./", "./.", "  ./  "] {
            assert_eq!(normalize(raw), "", "{raw:?}");
        }
    }

    #[test]
    fn what_changes_the_meaning_is_kept() {
        assert_eq!(normalize("../shared/./x"), "../shared/x");
        assert_eq!(normalize("a/../b"), "a/../b");
        assert_eq!(normalize("/etc//hosts"), "/etc/hosts");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize(".../x"), ".../x");
        assert_eq!(normalize("a..b/x"), "a..b/x");
        assert_eq!(normalize(".github/**"), ".github/**");
    }

    #[test]
    fn normalising_twice_changes_nothing() {
        for raw in [" ./a//b/./c/ ", "..", "/x/", "**/./*.rs", ""] {
            let once = normalize(raw);
            assert_eq!(normalize(&once), once, "{raw:?}");
        }
    }

    #[test]
    fn a_brace_makes_a_glob() {
        assert!(is_glob("config.{json,yaml}"));
        assert!(!is_glob("plain/path.txt"));
        assert!(!is_glob(""));
    }

    #[test]
    fn the_literal_prefix_stops_at_the_first_wildcard_segment() {
        assert_eq!(literal_prefix("src/{a,b}/x"), "src");
        assert_eq!(literal_prefix("*.txt"), "");
        assert_eq!(literal_prefix("a/b/c"), "a/b/c");
        assert_eq!(literal_prefix(""), "");
    }
}
