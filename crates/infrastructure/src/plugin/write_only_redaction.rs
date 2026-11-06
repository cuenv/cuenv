//! Provider-local protection for short and escaped write-only secret fragments.

use std::sync::RwLock;

#[derive(Default)]
pub(super) struct Redaction {
    patterns: RwLock<Vec<String>>,
}

impl std::fmt::Debug for Redaction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderRedaction")
            .finish_non_exhaustive()
    }
}

impl Redaction {
    pub(super) fn register(&self, value: &str) {
        let Ok(mut patterns) = self.patterns.write() else {
            return;
        };
        for part in std::iter::once(value)
            .chain(value.split(['\n', '\r']))
            .filter(|part| !part.is_empty())
        {
            let quoted = serde_json::to_string(part).unwrap_or_default();
            let quoted = quoted
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .unwrap_or("");
            let debug = format!("{part:?}");
            let debug = debug
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .unwrap_or("");
            let go = quoted
                .replace('&', "\\u0026")
                .replace('<', "\\u003c")
                .replace('>', "\\u003e")
                .replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029");
            for form in [part, quoted, debug, &go] {
                if !form.is_empty() && !patterns.iter().any(|pattern| pattern == form) {
                    patterns.push(form.to_string());
                }
            }
        }
    }

    pub(super) fn redact(&self, text: &str) -> String {
        let Ok(patterns) = self.patterns.read() else {
            return "*_*".to_string();
        };
        // Suppressing a matching string as a whole avoids exposing the tail of
        // overlapping secrets or disrupting the ordinary event redactor.
        if patterns
            .iter()
            .any(|pattern| text.contains(pattern.as_str()))
        {
            "*_*".to_string()
        } else {
            text.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Redaction;

    #[test]
    fn short_escaped_and_overlapping_fragments_suppress_the_whole_string() {
        let redaction = Redaction::default();
        for secret in ["bc", "bcdef", "x\nq\n<&>\\\"", "\u{2028}", "\u{2029}"] {
            redaction.register(secret);
        }
        for text in [
            "abcdef",
            "echo x",
            "echo q",
            "echo \\u2028",
            "echo \\u2029",
            "echo \\u003c\\u0026\\u003e\\\\\\\"",
        ] {
            assert_eq!(redaction.redact(text), "*_*");
        }
        assert_eq!(Redaction::default().redact("abcdef"), "abcdef");
    }
}
