//! Global secret redaction for cuenv events.
//!
//! Provides a centralized registry for secrets that should be redacted from all output.
//! Secrets are registered at runtime and automatically applied to event content
//! before events reach renderers.
//!
//! Redaction is a single pass over the text with a compiled multi-pattern
//! matcher. The matcher is built when text is first redacted after the
//! registry changed (the registry carries a generation counter) and shared
//! until the next change, so the cost of redacting a string does not grow
//! with the number of registered secrets and nothing is copied per call.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, PoisonError, RwLock};

use aho_corasick::{AhoCorasick, AhoCorasickKind, MatchKind};

/// Minimum secret length to redact (shorter secrets may cause false positives)
pub const MIN_SECRET_LENGTH: usize = 4;

/// Placeholder for redacted secrets
pub const REDACTED_PLACEHOLDER: &str = "*_*";

/// The registered secrets and the matcher compiled from them.
#[derive(Debug, Default)]
struct Registry {
    secrets: HashSet<String>,
    /// Incremented whenever `secrets` changes.
    generation: u64,
    compiled: Option<Compiled>,
}

/// A matcher and the registry generation it was built from.
#[derive(Debug)]
struct Compiled {
    generation: u64,
    /// `None` when the patterns could not be compiled; redaction then
    /// replaces the whole text rather than let a secret through.
    matcher: Option<Arc<AhoCorasick>>,
}

/// Global registry of secrets to redact
static SECRET_REGISTRY: LazyLock<RwLock<Registry>> =
    LazyLock::new(|| RwLock::new(Registry::default()));

/// What redaction should do with a text.
enum Matcher {
    /// No secret is registered.
    Nothing,
    /// The registered secrets could not be compiled into a matcher.
    Unavailable,
    Ready(Arc<AhoCorasick>),
}

impl Registry {
    fn insert(&mut self, secret: &str) {
        if insert_secret(&mut self.secrets, secret) {
            self.generation += 1;
        }
    }

    /// The matcher for the secrets as they are, if it is already built.
    fn current(&self) -> Option<Matcher> {
        if self.secrets.is_empty() {
            return Some(Matcher::Nothing);
        }
        self.compiled
            .as_ref()
            .filter(|compiled| compiled.generation == self.generation)
            .map(|compiled| {
                compiled
                    .matcher
                    .clone()
                    .map_or(Matcher::Unavailable, Matcher::Ready)
            })
    }

    fn compile(&mut self) -> Matcher {
        #[cfg(test)]
        test_support::COMPILATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let matcher = AhoCorasick::builder()
            // Every match, overlapping ones included: the redacted text is
            // the union of them, so a secret that overlaps another is not
            // left half visible.
            .match_kind(MatchKind::Standard)
            .kind(Some(AhoCorasickKind::NoncontiguousNFA))
            .build(&self.secrets)
            .ok()
            .map(Arc::new);
        self.compiled = Some(Compiled {
            generation: self.generation,
            matcher: matcher.clone(),
        });
        matcher.map_or(Matcher::Unavailable, Matcher::Ready)
    }
}

/// The matcher for the registry as it is now, compiled if it is not yet.
fn matcher() -> Matcher {
    let current = SECRET_REGISTRY
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .current();
    if let Some(current) = current {
        return current;
    }
    let mut registry = SECRET_REGISTRY
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    // Another thread may have compiled it while this one waited.
    registry.current().unwrap_or_else(|| registry.compile())
}

/// Register a secret value for redaction.
///
/// All future events will have this secret redacted from their content.
/// Secrets shorter than `MIN_SECRET_LENGTH` are ignored (too many false positives).
///
/// # Example
///
/// ```rust
/// use cuenv_events::redaction::register_secret;
///
/// // Register a secret for redaction
/// register_secret("my-secret-token-12345");
/// ```
pub fn register_secret(secret: impl Into<String>) {
    let secret = secret.into();
    SECRET_REGISTRY
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(&secret);
}

/// Add `secret` to the registry; whether the registry gained an entry.
///
/// A multi-line secret (a private key, a certificate) is registered line by
/// line: process output and logs are read and written one line at a time, so
/// no line of it ever contains the whole secret (see [`secret_forms`] for
/// when the whole text is registered too). A secret
/// with characters that are escaped when quoted (a quote, a backslash, a tab
/// or other control character) is also registered as it appears in quoted
/// text, in Rust's debug form and in JSON's, because plans and diagnostics
/// print values quoted. Go's JSON encoder, which Terraform providers log
/// with, additionally writes `&`, `<` and `>` as `\u0026`, `\u003c` and
/// `\u003e`, so that form is registered too.
fn insert_secret(registry: &mut HashSet<String>, secret: &str) -> bool {
    if secret.len() < MIN_SECRET_LENGTH {
        return false;
    }
    let before = registry.len();
    for form in secret_forms(secret) {
        if needs_quoted_forms(&form) {
            let json = json_quoted(&form);
            let go_json = go_escaped(&json);
            for quoted in [debug_quoted(&form), json, go_json] {
                if quoted != form {
                    registry.insert(quoted);
                }
            }
        }
        registry.insert(form);
    }
    registry.len() != before
}

/// The texts to look for to find `secret`: the secret itself, or, for a
/// multi-line secret, each of its lines (see [`insert_secret`]).
///
/// A multi-line secret is registered whole only when some line is too short
/// to be registered on its own: whenever every line is registered, the whole
/// text is already covered (line by line, and in its quoted forms, which
/// join the same lines with escapes), and registering it as well would
/// triple the matcher's size for a private key or a certificate.
fn secret_forms(secret: &str) -> Vec<String> {
    if !secret.contains(['\n', '\r']) {
        return vec![secret.to_string()];
    }
    let lines: Vec<&str> = secret.split(['\n', '\r']).collect();
    let mut forms: Vec<String> = lines
        .iter()
        .filter(|line| line.len() >= MIN_SECRET_LENGTH)
        .map(|line| (*line).to_string())
        .collect();
    let has_short_content = lines
        .iter()
        .any(|line| !line.is_empty() && line.len() < MIN_SECRET_LENGTH);
    if has_short_content || forms.is_empty() {
        forms.push(secret.to_string());
    }
    forms
}

/// Whether `text` can appear differently once quoted (in Rust's debug form,
/// in JSON, or in Go's JSON): anything but printable ASCII without a quote,
/// a backslash, `&`, `<` or `>`.
fn needs_quoted_forms(text: &str) -> bool {
    !text.bytes().all(|byte| {
        matches!(byte, 0x20..=0x7e) && !matches!(byte, b'"' | b'\\' | b'&' | b'<' | b'>')
    })
}

/// `text` as Rust's debug formatting writes it inside quotes.
fn debug_quoted(text: &str) -> String {
    let quoted = format!("{text:?}");
    strip_quotes(&quoted)
}

/// `text` as JSON writes it inside quotes.
fn json_quoted(text: &str) -> String {
    serde_json::to_string(text).map_or_else(|_| text.to_string(), |quoted| strip_quotes(&quoted))
}

/// `json` (already JSON-escaped text) with the extra escapes Go's
/// `encoding/json` applies to make JSON safe to embed in HTML.
fn go_escaped(json: &str) -> String {
    json.replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn strip_quotes(quoted: &str) -> String {
    quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(quoted)
        .to_string()
}

/// Register multiple secrets at once.
///
/// More efficient than calling `register_secret` multiple times.
///
/// # Example
///
/// ```rust
/// use cuenv_events::redaction::register_secrets;
///
/// register_secrets(["secret1", "secret2", "secret3"]);
/// ```
pub fn register_secrets(secrets: impl IntoIterator<Item = impl Into<String>>) {
    let mut registry = SECRET_REGISTRY
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    for secret in secrets {
        registry.insert(&secret.into());
    }
}

/// Redact all registered secrets from a string.
///
/// Returns the input with all registered secrets replaced with `*_*`.
/// Where secrets overlap, the whole overlapping stretch is replaced.
///
/// # Example
///
/// ```rust
/// use cuenv_events::redaction::{register_secret, redact};
///
/// register_secret("password123");
/// let redacted = redact("The password is password123");
/// assert!(redacted.contains("*_*"));
/// ```
#[must_use]
pub fn redact(input: &str) -> String {
    redact_cow(input).into_owned()
}

/// [`redact`] without copying text that holds no secret.
#[must_use]
pub fn redact_cow(input: &str) -> Cow<'_, str> {
    match matcher() {
        Matcher::Nothing => Cow::Borrowed(input),
        Matcher::Unavailable => Cow::Borrowed(REDACTED_PLACEHOLDER),
        Matcher::Ready(matcher) => replace_matches(&matcher, input),
    }
}

/// `input` with the union of every match replaced by the placeholder.
fn replace_matches<'text>(matcher: &AhoCorasick, input: &'text str) -> Cow<'text, str> {
    let mut spans: Vec<(usize, usize)> = matcher
        .find_overlapping_iter(input)
        .map(|found| (found.start(), found.end()))
        .collect();
    spans.sort_unstable();
    let mut spans = spans.into_iter();
    let Some((mut start, mut end)) = spans.next() else {
        return Cow::Borrowed(input);
    };
    let mut result = String::with_capacity(input.len());
    let mut copied = 0;
    for (next_start, next_end) in spans {
        if next_start < end {
            end = end.max(next_end);
        } else {
            result.push_str(&input[copied..start]);
            result.push_str(REDACTED_PLACEHOLDER);
            copied = end;
            (start, end) = (next_start, next_end);
        }
    }
    result.push_str(&input[copied..start]);
    result.push_str(REDACTED_PLACEHOLDER);
    result.push_str(&input[end..]);
    Cow::Owned(result)
}

/// What redaction does with the keys of JSON objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyHandling {
    Keep,
    Redact,
}

/// Redact every registered secret from each string value inside a JSON
/// value, in place. Object keys are left as they are.
///
/// Redaction acts on the strings themselves, never on serialized text: a
/// secret containing a quote, a backslash or a newline is written escaped
/// in serialized JSON (`quo\"te`), where a search for the raw secret would
/// not find it. Keys are not touched because in a fixed schema they are
/// names the reader depends on; a secret that equals a key must not rename
/// it. For JSON whose keys are content too, use
/// [`redact_json_value_and_keys`].
pub fn redact_json_value(value: &mut serde_json::Value) {
    redact_json(value, KeyHandling::Keep);
}

/// Like [`redact_json_value`], and the keys of objects are redacted as well.
///
/// For JSON that is not a schema cuenv defines (what a provider logs, a
/// map a user wrote), where a key can be content.
pub fn redact_json_value_and_keys(value: &mut serde_json::Value) {
    redact_json(value, KeyHandling::Redact);
}

fn redact_json(value: &mut serde_json::Value, keys: KeyHandling) {
    match value {
        serde_json::Value::String(text) => {
            if let Cow::Owned(redacted) = redact_cow(text) {
                *text = redacted;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_json(item, keys);
            }
        }
        serde_json::Value::Object(map) => match keys {
            KeyHandling::Keep => {
                for entry in map.values_mut() {
                    redact_json(entry, keys);
                }
            }
            KeyHandling::Redact => {
                let entries = std::mem::take(map);
                for (key, mut entry) in entries {
                    redact_json(&mut entry, keys);
                    map.insert(redact_cow(&key).into_owned(), entry);
                }
            }
        },
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Redact every registered secret from one line of JSON text.
///
/// Text that parses as JSON is redacted string by string (see
/// [`redact_json_value`]; keys are kept) and written again compactly;
/// anything else is redacted as plain text.
#[must_use]
pub fn redact_json_text(text: &str) -> String {
    redact_json_line(text, KeyHandling::Keep)
}

/// Like [`redact_json_text`], and the keys of objects are redacted as well
/// (see [`redact_json_value_and_keys`]).
#[must_use]
pub fn redact_free_form_json_text(text: &str) -> String {
    redact_json_line(text, KeyHandling::Redact)
}

fn redact_json_line(text: &str, keys: KeyHandling) -> String {
    if !has_secrets() {
        return text.to_string();
    }
    let trimmed = text.trim_end_matches(['\n', '\r']);
    let line_ending = &text[trimmed.len()..];
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(mut value) => {
            redact_json(&mut value, keys);
            match serde_json::to_string(&value) {
                Ok(json) => format!("{json}{line_ending}"),
                Err(_) => redact(text),
            }
        }
        Err(_) => redact(text),
    }
}

/// Check if any secrets are registered.
#[must_use]
pub fn has_secrets() -> bool {
    !SECRET_REGISTRY
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .secrets
        .is_empty()
}

/// Get the number of registered secrets.
#[must_use]
pub fn secret_count() -> usize {
    SECRET_REGISTRY
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .secrets
        .len()
}

/// Clear all registered secrets.
///
/// This is primarily useful for testing to ensure test isolation.
#[cfg(test)]
pub fn clear_secrets() {
    let mut registry = SECRET_REGISTRY
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    registry.secrets.clear();
    registry.generation += 1;
    registry.compiled = None;
}

/// Serializes the tests that share the process-wide registry.
#[cfg(test)]
pub(crate) mod test_support {
    use super::clear_secrets;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    /// How many times the matcher was compiled.
    pub(super) static COMPILATIONS: AtomicUsize = AtomicUsize::new(0);

    // Use a mutex to ensure tests don't interfere with each other
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    pub fn with_clean_registry<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear_secrets();
        let result = f();
        clear_secrets();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{COMPILATIONS, with_clean_registry};
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn the_matcher_is_compiled_once_per_registry_change_not_once_per_call() {
        with_clean_registry(|| {
            register_secrets(["first-secret-AAAA", "second-secret-BBBB"]);
            let before = COMPILATIONS.load(Ordering::Relaxed);
            for _ in 0..50 {
                assert_eq!(redact("x first-secret-AAAA y"), "x *_* y");
            }
            assert_eq!(
                COMPILATIONS.load(Ordering::Relaxed),
                before + 1,
                "50 calls on an unchanged registry compile once"
            );
            // Registering a known secret again changes nothing.
            register_secret("first-secret-AAAA");
            assert_eq!(redact("first-secret-AAAA"), "*_*");
            assert_eq!(COMPILATIONS.load(Ordering::Relaxed), before + 1);
            // A new secret does.
            register_secret("third-secret-CCCC");
            assert_eq!(redact("third-secret-CCCC"), "*_*");
            assert_eq!(COMPILATIONS.load(Ordering::Relaxed), before + 2);
        });
    }

    #[test]
    fn many_long_secrets_do_not_slow_each_redaction_down() {
        with_clean_registry(|| {
            // 200 secrets of 100 lines of 64 characters, as a large plan with
            // many multi-line secrets registers them.
            let secrets: Vec<String> = (0..200)
                .map(|secret| {
                    (0..100)
                        .map(|line| format!("{secret:04}-{line:04}-{}", "x".repeat(54)))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect();
            register_secrets(secrets.iter().cloned());
            // Compile outside the timed part.
            assert_eq!(redact("warm up"), "warm up");
            let started = std::time::Instant::now();
            for _ in 0..20_000 {
                assert_eq!(
                    redact("provider log: refreshing resource"),
                    "provider log: refreshing resource"
                );
            }
            let elapsed = started.elapsed();
            assert!(
                elapsed < std::time::Duration::from_secs(10),
                "20,000 redactions took {elapsed:?}"
            );
            // Registered line by line (every line is long enough to be), so
            // the whole secret comes out as its lines replaced one by one.
            let leaked = redact(&format!("leak {}", secrets[7]));
            assert!(!leaked.contains("0007-"), "{leaked}");
            assert_eq!(leaked.matches("*_*").count(), 100, "{leaked}");
        });
    }

    #[test]
    fn a_multi_line_secret_is_registered_whole_only_when_a_line_is_too_short_to_be() {
        with_clean_registry(|| {
            // Every line long enough: the lines cover the whole, and plain
            // text has no quoted forms, so three patterns suffice.
            register_secret("first-line-AAAA\nsecond-line-BBBB\nthird-line-CCCC");
            assert_eq!(secret_count(), 3);
            assert_eq!(
                redact("first-line-AAAA\nsecond-line-BBBB"),
                "*_*\n*_*",
                "the whole text is still hidden"
            );
        });
        with_clean_registry(|| {
            // A short line is never registered alone, so the whole is.
            register_secret("first-line-AAAA\nxy\nthird-line-CCCC");
            assert!(redact("first-line-AAAA\nxy\nthird-line-CCCC").contains("*_*"));
            assert_eq!(redact("a lone xy"), "a lone xy");
            assert_eq!(redact("first-line-AAAA\nxy\nthird-line-CCCC"), "*_*");
        });
    }

    #[test]
    fn overlapping_secrets_are_replaced_as_one_stretch() {
        with_clean_registry(|| {
            register_secrets(["abcdXXXX", "XXXXefgh"]);
            // Each secret overlaps the other: no part of either may show.
            assert_eq!(redact("<abcdXXXXefgh>"), "<*_*>");
            // Adjacent, not overlapping: two placeholders.
            assert_eq!(redact("abcdXXXX-XXXXefgh"), "*_*-*_*");
        });
    }

    #[test]
    fn go_escaped_json_forms_of_a_secret_are_redacted() {
        with_clean_registry(|| {
            register_secret("p&ss<w>rd-GGGG");
            // Go's encoding/json writes &, < and > as \u0026, \u003c and \u003e.
            assert_eq!(
                redact(r#"{"@message":"token p\u0026ss\u003cw\u003erd-GGGG"}"#),
                r#"{"@message":"token *_*"}"#
            );
            // The decoded form of the same line.
            assert_eq!(redact("token p&ss<w>rd-GGGG"), "token *_*");
        });
    }

    #[test]
    fn json_object_keys_are_kept_unless_asked_for() {
        with_clean_registry(|| {
            register_secret("data");
            let mut value = serde_json::json!({"data": {"content": "data", "other": ["data"]}});
            redact_json_value(&mut value);
            assert_eq!(
                value,
                serde_json::json!({"data": {"content": "*_*", "other": ["*_*"]}})
            );
            let mut value = serde_json::json!({"data": "x"});
            redact_json_value_and_keys(&mut value);
            assert_eq!(value, serde_json::json!({"*_*": "x"}));
            assert_eq!(
                redact_json_text("{\"data\":\"data\"}\n"),
                "{\"data\":\"*_*\"}\n"
            );
            assert_eq!(
                redact_free_form_json_text("{\"data\":\"data\"}"),
                "{\"*_*\":\"*_*\"}"
            );
        });
    }

    #[test]
    fn text_without_a_secret_is_not_copied() {
        with_clean_registry(|| {
            register_secret("needle-NNNN");
            assert!(matches!(redact_cow("haystack"), Cow::Borrowed(_)));
            assert!(matches!(redact_cow("a needle-NNNN"), Cow::Owned(_)));
        });
    }

    #[test]
    fn each_line_of_a_multi_line_secret_is_redacted_on_its_own() {
        with_clean_registry(|| {
            register_secret("line-one-MMMM\nline-two-MMMM\r\nxy");
            // Output is read line by line: no line holds the whole secret.
            assert_eq!(redact("log: line-two-MMMM"), "log: *_*");
            assert_eq!(redact("line-one-MMMM"), "*_*");
            // Lines shorter than the minimum are not registered.
            assert_eq!(redact("xy"), "xy");
        });
    }

    #[test]
    fn a_secret_is_also_redacted_as_quoted_text_prints_it() {
        with_clean_registry(|| {
            register_secret("quo\"te\\back-QQQQ");
            register_secret("tab\there-UUUU");
            // How a plan or a diagnostic prints the values (debug quoting).
            assert_eq!(
                redact(&format!("name = {:?}", "quo\"te\\back-QQQQ")),
                "name = \"*_*\""
            );
            assert_eq!(redact(&format!("{:?}", "tab\there-UUUU")), "\"*_*\"");
            // And as JSON text holds them.
            assert_eq!(
                redact(&serde_json::json!("tab\there-UUUU").to_string()),
                "\"*_*\""
            );
        });
    }

    #[test]
    fn json_strings_are_redacted_in_their_unescaped_form() {
        with_clean_registry(|| {
            register_secret("quo\"te\\back-QQQQ");
            let mut value = serde_json::json!({
                "message": "open /x/quo\"te\\back-QQQQ/y",
                "nested": [{"key-quo\"te\\back-QQQQ": "ok"}],
                "count": 3
            });
            redact_json_value_and_keys(&mut value);
            let serialized = value.to_string();
            assert!(!serialized.contains("QQQQ"), "{serialized}");
            assert_eq!(value["message"], "open /x/*_*/y");
            assert_eq!(value["count"], 3);
        });
    }

    #[test]
    fn json_text_is_redacted_per_string_and_other_text_as_plain_text() {
        with_clean_registry(|| {
            register_secret("quo\"te-QQQQ");
            let line = serde_json::json!({"m": "a quo\"te-QQQQ b"}).to_string();
            assert!(line.contains("quo\\\"te-QQQQ"), "escaped form: {line}");
            assert_eq!(
                redact_json_text(&format!("{line}\n")),
                "{\"m\":\"a *_* b\"}\n"
            );
            assert_eq!(redact_json_text("not json quo\"te-QQQQ"), "not json *_*");
        });
    }

    #[test]
    fn test_simple_redaction() {
        with_clean_registry(|| {
            register_secret("secret123");
            let result = redact("The password is secret123, don't share it");
            assert_eq!(result, "The password is *_*, don't share it");
        });
    }

    #[test]
    fn test_multiple_secrets() {
        with_clean_registry(|| {
            register_secrets(["password123", "api_key_xyz"]);
            let result = redact("password123 and api_key_xyz are both secrets");
            assert_eq!(result, "*_* and *_* are both secrets");
        });
    }

    #[test]
    fn test_repeated_secret() {
        with_clean_registry(|| {
            register_secret("secret");
            let result = redact("secret appears twice: secret");
            assert_eq!(result, "*_* appears twice: *_*");
        });
    }

    #[test]
    fn test_short_secret_ignored() {
        with_clean_registry(|| {
            register_secret("ab"); // Too short (< 4 chars)
            register_secret("abc"); // Too short
            register_secret("abcd"); // Just right (= 4 chars)

            assert_eq!(secret_count(), 1);

            let result = redact("ab abc abcd");
            assert_eq!(result, "ab abc *_*");
        });
    }

    #[test]
    fn test_empty_input() {
        with_clean_registry(|| {
            register_secret("secret");
            let result = redact("");
            assert_eq!(result, "");
        });
    }

    #[test]
    fn test_no_secrets_registered() {
        with_clean_registry(|| {
            assert!(!has_secrets());
            let result = redact("nothing to redact here");
            assert_eq!(result, "nothing to redact here");
        });
    }

    #[test]
    fn test_greedy_matching() {
        with_clean_registry(|| {
            // Longer secret should be matched first
            register_secrets(["pass", "password"]);
            let result = redact("the password is set");
            // Should redact "password" not just "pass"
            assert_eq!(result, "the *_* is set");
        });
    }

    #[test]
    fn test_secret_at_boundaries() {
        with_clean_registry(|| {
            register_secret("secret");

            // Secret at start
            let result = redact("secret is here");
            assert_eq!(result, "*_* is here");

            // Secret at end
            let result = redact("here is secret");
            assert_eq!(result, "here is *_*");
        });
    }

    #[test]
    fn test_special_characters() {
        with_clean_registry(|| {
            register_secret("pass$word!@#");
            let result = redact("the pass$word!@# is special");
            assert_eq!(result, "the *_* is special");
        });
    }

    #[test]
    fn test_multiline_content() {
        with_clean_registry(|| {
            register_secret("secretkey");
            let input = "line1\nsecretkey\nline3";
            let result = redact(input);
            assert_eq!(result, "line1\n*_*\nline3");
        });
    }

    #[test]
    fn test_has_secrets() {
        with_clean_registry(|| {
            assert!(!has_secrets());
            register_secret("test_secret");
            assert!(has_secrets());
        });
    }

    #[test]
    fn test_secret_count() {
        with_clean_registry(|| {
            assert_eq!(secret_count(), 0);
            register_secret("secret1");
            assert_eq!(secret_count(), 1);
            register_secret("secret2");
            assert_eq!(secret_count(), 2);
            // Duplicate should not increase count
            register_secret("secret1");
            assert_eq!(secret_count(), 2);
        });
    }
}
