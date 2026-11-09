//! Console output with registered secrets redacted.

use std::io::{self, Write};

use crate::redaction;

/// Print to stdout with automatic secret redaction (with newline).
///
/// Use this instead of `println!` when output might contain secrets.
/// This function applies `redact()` to the input before printing,
/// ensuring any registered secrets are replaced with `*_*`.
pub fn println_redacted(content: &str) {
    let stdout = io::stdout();
    writeln_redacted_to(stdout.lock(), content);
}

/// Print to stdout with automatic secret redaction (no newline).
///
/// Use this instead of `print!` when output might contain secrets.
pub fn print_redacted(content: &str) {
    let stdout = io::stdout();
    write_redacted_to(stdout.lock(), content);
}

/// Print to stderr with automatic secret redaction (with newline).
///
/// Use this instead of `eprintln!` when output might contain secrets.
pub fn eprintln_redacted(content: &str) {
    let stderr = io::stderr();
    writeln_redacted_to(stderr.lock(), content);
}

/// Print to stderr with automatic secret redaction (no newline).
///
/// Use this instead of `eprint!` when output might contain secrets.
pub fn eprint_redacted(content: &str) {
    let stderr = io::stderr();
    write_redacted_to(stderr.lock(), content);
}

fn writeln_redacted_to(mut writer: impl Write, content: &str) {
    if let Err(error) = writeln!(writer, "{}", redaction::redact(content)) {
        log_redacted_write_error(&error);
    }
}

fn write_redacted_to(mut writer: impl Write, content: &str) {
    if let Err(error) = write!(writer, "{}", redaction::redact(content)) {
        log_redacted_write_error(&error);
    }
}

fn log_redacted_write_error(error: &io::Error) {
    tracing::debug!(%error, "failed to write redacted output");
}

#[cfg(test)]
mod tests {
    use crate::redaction;

    #[test]
    fn redacted_writers_apply_redaction() {
        redaction::test_support::with_clean_registry(|| {
            redaction::register_secret("writer-secret-token");

            let mut line_output = Vec::new();
            super::writeln_redacted_to(&mut line_output, "value=writer-secret-token");
            assert_eq!(line_output, b"value=*_*\n");

            let mut output = Vec::new();
            super::write_redacted_to(&mut output, "value=writer-secret-token");
            assert_eq!(output, b"value=*_*");
        });
    }
}
