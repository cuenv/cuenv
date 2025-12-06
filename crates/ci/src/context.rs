#[derive(Debug, Clone)]
pub struct CIContext {
    pub provider: String,
    pub event: String,
    pub ref_name: String,
    pub base_ref: Option<String>,
    pub sha: String,
}
