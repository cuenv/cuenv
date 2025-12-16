use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PipelineCondition {
    pub pull_request: Option<bool>,
    #[serde(default)]
    pub branch: Option<StringOrVec>,
    #[serde(default)]
    pub tag: Option<StringOrVec>,
    pub default_branch: Option<bool>,
    pub scheduled: Option<bool>,
    pub manual: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pipeline {
    pub name: String,
    pub when: Option<PipelineCondition>,
    pub tasks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CI {
    pub pipelines: Vec<Pipeline>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StringOrVec {
    String(String),
    Vec(Vec<String>),
}
