use serde::{Deserialize, Serialize};

/// 喂给匹配 prompt 的 MAL 侧信息（前三个字段 low/high 都用，其余仅 high 模式进 prompt）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MalBrief {
    pub title: String,
    pub title_ja: Option<String>,
    pub media_type: Option<String>,
    pub start_date: Option<String>,
    pub num_episodes: Option<i32>,
    pub source: Option<String>,
    pub studios: Option<Vec<String>>,
    pub synopsis: Option<String>,
}

/// 喂给匹配 prompt 的单个 BGM 候选（`platform`/`tags` 仅 high 模式进 prompt）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BgmCandidate {
    pub bgm_id: i32,
    pub name: String,
    pub name_cn: Option<String>,
    pub date: Option<String>,
    pub summary: Option<String>,
    pub platform: Option<String>,
    pub tags: Option<Vec<String>>,
}

/// 模型对单个候选的打分。
#[derive(Debug, Clone, PartialEq)]
pub struct LlmMatch {
    pub bgm_id: i32,
    pub confidence: Option<f64>,
    pub reason: Option<String>,
}

/// `suggest_search` 的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum Suggest {
    Keywords(Vec<String>),
    Skip,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChatMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: f32,
    pub max_tokens: u32,
    pub stream: bool,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ChatResponse {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatChoice {
    #[serde(default)]
    pub message: Option<ChatChoiceMessage>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatChoiceMessage {
    #[serde(default)]
    pub content: Option<String>,
}
