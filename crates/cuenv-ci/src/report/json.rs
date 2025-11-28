use super::PipelineReport;
use cuenv_core::Result;

/// Writes the pipeline report to a JSON file
///
/// # Errors
/// Returns error if file creation or JSON serialization fails
pub fn write_report(report: &PipelineReport, path: &std::path::Path) -> Result<()> {
    let file = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(file, report).map_err(|e| cuenv_core::Error::Io {
        source: e.into(),
        path: Some(path.into()),
        operation: "write_report".to_string(),
    })?;
    Ok(())
}
