//! A writer that redacts registered secrets from what it writes.
//!
//! Tracing's formatting layers write straight to standard error and know
//! nothing about the secret registry. [`RedactingStderr`] is the writer to
//! hand them: each log record is buffered, redacted as a whole (so a secret
//! cannot be split across writes), and only then written.

use std::io::{self, Write};

use tracing_subscriber::fmt::MakeWriter;

use crate::redaction::{redact, redact_json_text};

/// How the text a [`RedactingWriter`] receives is structured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Plain text: secrets are replaced wherever they appear.
    Text,
    /// One JSON document per line: secrets are replaced inside each string,
    /// so a secret that JSON escapes (a quote, a backslash, a newline) is
    /// found in its unescaped form.
    JsonLines,
}

/// Buffers everything written to it and, when flushed or dropped, writes it
/// to the inner writer with every registered secret replaced.
#[derive(Debug)]
pub struct RedactingWriter<Inner: Write> {
    inner: Inner,
    format: LogFormat,
    buffer: Vec<u8>,
}

impl<Inner: Write> RedactingWriter<Inner> {
    /// A redacting writer in front of `inner`.
    #[must_use]
    pub fn new(inner: Inner, format: LogFormat) -> Self {
        Self {
            inner,
            format,
            buffer: Vec::new(),
        }
    }

    fn write_buffered(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buffer).into_owned();
        self.buffer.clear();
        let redacted = match self.format {
            LogFormat::Text => redact(&text),
            LogFormat::JsonLines => text
                .split_inclusive('\n')
                .map(redact_json_text)
                .collect::<String>(),
        };
        self.inner.write_all(redacted.as_bytes())
    }
}

impl<Inner: Write> Write for RedactingWriter<Inner> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.write_buffered()?;
        self.inner.flush()
    }
}

impl<Inner: Write> Drop for RedactingWriter<Inner> {
    fn drop(&mut self) {
        // A failed write to standard error has nowhere to be reported.
        let _ = self.flush();
    }
}

/// Makes a [`RedactingWriter`] in front of standard error for each log
/// record; pass it to a tracing formatting layer with `with_writer`.
#[derive(Debug, Clone, Copy)]
pub struct RedactingStderr {
    format: LogFormat,
}

impl RedactingStderr {
    /// A maker for records in the given format.
    #[must_use]
    pub const fn new(format: LogFormat) -> Self {
        Self { format }
    }
}

impl<'writer> MakeWriter<'writer> for RedactingStderr {
    type Writer = RedactingWriter<io::Stderr>;

    fn make_writer(&'writer self) -> Self::Writer {
        RedactingWriter::new(io::stderr(), self.format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redaction::{register_secret, test_support::with_clean_registry};

    fn written(format: LogFormat, chunks: &[&str]) -> String {
        let mut output = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut output, format);
            for chunk in chunks {
                writer.write_all(chunk.as_bytes()).unwrap();
            }
        }
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn a_secret_split_across_writes_is_redacted() {
        with_clean_registry(|| {
            register_secret("split-secret-value");
            let output = written(LogFormat::Text, &["token=split-sec", "ret-value\n"]);
            assert_eq!(output, "token=*_*\n");
        });
    }

    #[test]
    fn json_log_lines_are_redacted_inside_their_strings() {
        with_clean_registry(|| {
            register_secret("quo\"te-QQQQ");
            let line = serde_json::json!({"fields": {"message": "path /x/quo\"te-QQQQ/y"}});
            let output = written(LogFormat::JsonLines, &[&format!("{line}\n")]);
            assert!(!output.contains("QQQQ"), "{output}");
            assert!(output.ends_with('\n'));
            let parsed: serde_json::Value = serde_json::from_str(output.trim_end()).unwrap();
            assert_eq!(parsed["fields"]["message"], "path /x/*_*/y");
        });
    }

    #[test]
    fn nothing_is_written_when_nothing_was() {
        with_clean_registry(|| {
            assert_eq!(written(LogFormat::Text, &[]), "");
        });
    }
}
