use super::PipelineReport;

#[must_use]
pub fn generate_summary(report: &PipelineReport) -> String {
    // TODO: Implement markdown generation
    format!(
        "## cuenv: {}\n\nStatus: {:?}",
        report.project, report.status
    )
}
