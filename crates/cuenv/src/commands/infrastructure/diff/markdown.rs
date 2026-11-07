//! The Markdown a diff prints: a summary line, then one section each for
//! providers, managed resources and data sources that changed.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use super::changes::{Change, Changes, FieldChange, Segment};
use super::snapshot::{Output, Secret, Value};

/// A rendered value longer than this is shortened, with its length and digest.
const LONGEST_VALUE: usize = 200;

/// How much of a shortened value is shown.
const SHORTENED_PREFIX: usize = 120;

/// What the diff was compared with, for the summary line.
pub(super) struct Comparison<'comparison> {
    /// The base revision as given, such as `origin/main`.
    pub(super) base: &'comparison str,
    /// The commit it resolved to.
    pub(super) commit: &'comparison str,
    /// The selected infrastructure environment, if any.
    pub(super) environment: Option<&'comparison str>,
}

/// Render the changes as Markdown.
pub(super) fn render(changes: &Changes, comparison: &Comparison<'_>) -> String {
    let mut text = String::new();
    let mut compared = format!(
        "compared with {} ({})",
        code(comparison.base),
        code(short_commit(comparison.commit))
    );
    if let Some(environment) = comparison.environment {
        let _ = write!(compared, " in environment {}", code(environment));
    }
    if changes.is_empty() {
        let _ = writeln!(text, "No infrastructure changes {compared}.");
        return text;
    }
    let sections = [
        ("Providers", &changes.providers),
        ("Resources", &changes.resources),
        ("Data sources", &changes.data_sources),
    ];
    let summary: Vec<String> = sections
        .iter()
        .filter(|(_, section)| !section.is_empty())
        .map(|(title, section)| format!("{title}: {}", counts(section)))
        .collect();
    let _ = writeln!(
        text,
        "Infrastructure changes {compared}. {}.",
        summary.join("; ")
    );
    for (title, section) in sections {
        if section.is_empty() {
            continue;
        }
        let _ = write!(text, "\n#### {title}\n\n");
        for (address, change) in section {
            render_change(&mut text, address, change);
        }
    }
    text
}

fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// "1 added, 2 changed", leaving out what did not happen.
fn counts(section: &BTreeMap<String, Change>) -> String {
    let count =
        |wanted: fn(&Change) -> bool| section.values().filter(|change| wanted(change)).count();
    [
        (count(|change| matches!(change, Change::Added(_))), "added"),
        (
            count(|change| matches!(change, Change::Changed(_))),
            "changed",
        ),
        (count(|change| matches!(change, Change::Removed)), "removed"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, verb)| format!("{count} {verb}"))
    .collect::<Vec<_>>()
    .join(", ")
}

fn render_change(text: &mut String, address: &str, change: &Change) {
    match change {
        Change::Added(fields) => {
            let _ = writeln!(text, "- **added** {}", code(address));
            for (path, value) in fields {
                let _ = writeln!(text, "  - {}: {}", code(&path_text(path)), markdown(value));
            }
        }
        Change::Changed(fields) => {
            let _ = writeln!(text, "- **changed** {}", code(address));
            for field in fields {
                let _ = writeln!(text, "  - {}", field_change(field));
            }
        }
        Change::Removed => {
            let _ = writeln!(text, "- **removed** {}", code(address));
        }
    }
}

fn field_change(change: &FieldChange) -> String {
    let path = code(&path_text(&change.path));
    match (&change.before, &change.after) {
        (Some(before), Some(after)) => {
            format!("{path}: {} → {}", markdown(before), markdown(after))
        }
        (None, Some(after)) => format!("{path}: added {}", markdown(after)),
        (Some(before), None) => format!("{path}: removed (was {})", markdown(before)),
        (None, None) => path,
    }
}

/// A path such as `configuration.rules[0]."example.com"`.
pub(super) fn path_text(path: &[Segment]) -> String {
    let mut text = String::new();
    for segment in path {
        match segment {
            Segment::Field(name) => {
                if !text.is_empty() {
                    text.push('.');
                }
                text.push_str(&key(name));
            }
            Segment::Index(index) => {
                let _ = write!(text, "[{index}]");
            }
        }
    }
    text
}

/// A key as written: bare when it is an identifier, quoted otherwise.
fn key(name: &str) -> String {
    let mut characters = name.chars();
    let identifier = characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_');
    if identifier {
        name.to_string()
    } else {
        Json::String(name.to_string()).to_string()
    }
}

/// A value as inline Markdown: in a code span, shortened when long.
fn markdown(value: &Value) -> String {
    let text = inline(value);
    let length = text.chars().count();
    if length <= LONGEST_VALUE {
        return code(&text);
    }
    let prefix: String = text.chars().take(SHORTENED_PREFIX).collect();
    let digest = hex::encode(Sha256::digest(text.as_bytes()));
    format!(
        "{} ({length} characters, sha256 {})",
        code(&format!("{prefix}…")),
        code(&digest[..12])
    )
}

/// A value on one line: JSON scalars, secrets and output references in
/// parentheses, lists in brackets and objects in braces.
pub(super) fn inline(value: &Value) -> String {
    match value {
        Value::Scalar(scalar) => scalar.to_string(),
        Value::Secret(secret) => secret_text(secret),
        Value::Output(output) => output_text(output),
        Value::List(items) => format!(
            "[{}]",
            items.iter().map(inline).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(name, value)| format!("{}: {}", key(name), inline(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `(secret onepassword: ref="op://vault/item/field")`: the resolver and
/// what it reads, never a value.
fn secret_text(secret: &Secret) -> String {
    let kind = if secret.write_only {
        "write-only secret"
    } else {
        "secret"
    };
    if secret.fields.is_empty() {
        return format!("({kind} {})", secret.resolver);
    }
    let fields: Vec<String> = secret
        .fields
        .iter()
        .map(|(name, value)| format!("{}={value}", key(name)))
        .collect();
    format!("({kind} {}: {})", secret.resolver, fields.join(", "))
}

/// `(output of cloudflare_r2_bucket.bucket: name)`, or what a computed value
/// is computed from.
fn output_text(output: &Output) -> String {
    let sources: Vec<String> = output
        .sources
        .iter()
        .map(|source| format!("{}: {}", source.address, source.attribute))
        .collect();
    match (output.computed, sources.as_slice()) {
        (false, [source]) => format!("(output of {source})"),
        (true, [source]) => format!("(computed from output of {source})"),
        _ => format!("(computed from outputs of {})", sources.join(", ")),
    }
}

/// A code span that shows `text` exactly, whatever backticks it holds.
pub(super) fn code(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for character in text.chars() {
        if character == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    let padded = text.starts_with('`')
        || text.ends_with('`')
        || (text.starts_with(' ') && text.ends_with(' ') && !text.trim().is_empty());
    if padded {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}
