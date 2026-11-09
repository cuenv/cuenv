//! JSON serialization with an enforced output-size limit.

use std::io::Write;

use crate::{Error, Result};

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|total| total > self.limit)
        {
            return Err(std::io::Error::other(
                "hook data exceeds the serialization limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn serialize_bounded(value: &impl serde::Serialize, limit: usize) -> Result<Vec<u8>> {
    let mut output = LimitedWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value)
        .map_err(|_| Error::configuration("Hook data exceeds the serialization limit"))?;
    Ok(output.bytes)
}
