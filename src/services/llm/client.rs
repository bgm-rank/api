use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::{Client, header};

use super::schemas::*;

const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
const OPENROUTER_DEFAULT_MODEL: &str = "google/gemini-2.5-flash-lite";
const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";
const DEEPSEEK_DEFAULT_MODEL: &str = "deepseek-chat";
const MAX_ATTEMPTS: u32 = 3;
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const SUMMARY_MAX_CHARS: usize = 80;
const SUMMARY_MAX_CHARS_HIGH: usize = 240;
const SYNOPSIS_MAX_CHARS_HIGH: usize = 300;
const MAX_TAGS_HIGH: usize = 5;
const REASON_MAX_CHARS: usize = 60;

// 推理模型的 reasoning_tokens 也占 max_tokens，务必留足余量（>= 1024）。
const MAX_TOKENS_MATCH: u32 = 2048;
const MAX_TOKENS_MATCH_HIGH: u32 = 3072;
const MAX_TOKENS_SUGGEST: u32 = 2048;

const MATCH_SYSTEM_PROMPT: &str = concat!(
    "匹配MAL动漫与Bangumi候选。续作必须季数一致（2nd/第2期/II等）。",
    "MAL格式：英文名|日文名|媒体类型。BGM候选格式：bgm_id:日文名|中文名|首播日期|简介(可选)。",
    "输出JSON数组，每个候选含bgm_id和confidence(0.0-1.0)，无匹配输出空数组：",
    r#"[{"bgm_id":数字,"confidence":0.9}]"#
);

const MATCH_SYSTEM_PROMPT_HIGH: &str = concat!(
    "匹配MAL动漫与Bangumi候选。判定规则：\n",
    "1.续作季数必须一致（2nd/第2期/II等），季数不同判否。\n",
    "2.首播日期应接近，相差超过半年需有同一作品不同发行的确证才可匹配。\n",
    "3.媒体类型与BGM platform冲突（movie对TV连载、TV对剧场版）判否。\n",
    "4.简介须描述同一作品：角色、设定、剧情一致；同系列的不同作品判否。\n",
    "MAL格式：英文名|日文名|媒体类型|首播日期|集数|原作|制作公司|简介\n",
    "BGM候选格式：bgm_id:日文名|中文名|首播日期|platform|tags|简介\n",
    "（字段缺失时该段省略，不留空管道）\n",
    "输出JSON数组，按confidence降序，无匹配输出空数组：\n",
    r#"[{"bgm_id":数字,"confidence":0.9,"reason":"20字以内的判定依据"}]"#
);

const SUGGEST_SYSTEM_PROMPT: &str = concat!(
    "从MAL动画标题提取用于Bangumi(日本动画数据库)搜索的关键词。只返回JSON。\n",
    "规则:\n",
    r#"1.JA标题含韩文(한글)或全中文→{"skip":true}"#,
    "\n",
    "2.否则从JA标题提取关键词:去掉劇場版/映画前缀、续集标记(第N期/Season)、副标题(～xx～/-xx-/『』)，",
    r#"保留最具辨识度的日文部分→{"keywords":["关键词"]}"#,
    "\n例:\n",
    r#"JA:新劇場版 銀魂 -吉原大炎上-→{"keywords":["銀魂 吉原大炎上"]}"#,
    "\n",
    r#"JA:프린세스 캐치! 티니핑→{"skip":true}"#,
    "\n",
    r#"JA:劇場版『ゾンビランドサガ ゆめぎんがパラダイス』→{"keywords":["ゾンビランドサガ"]}"#
);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// LLM 匹配能力抽象，便于 `MatcherService` 注入 fake 做测试。
pub trait LlmMatcher: Send + Sync {
    fn match_anime<'a>(
        &'a self,
        mal: &'a MalBrief,
        candidates: &'a [BgmCandidate],
    ) -> BoxFuture<'a, Result<Vec<LlmMatch>>>;

    fn suggest_search<'a>(
        &'a self,
        title_ja: &'a str,
        media_type: &'a str,
    ) -> BoxFuture<'a, Result<Suggest>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptMode {
    #[default]
    Low,
    High,
}

impl PromptMode {
    /// 解析 `LLM_PROMPT_MODE`；未知值退回 low。
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_lowercase()).as_deref() {
            None | Some("") | Some("low") => Self::Low,
            Some("high") => Self::High,
            Some(other) => {
                tracing::warn!(mode = other, "未知的 LLM_PROMPT_MODE，退回 low");
                Self::Low
            }
        }
    }
}

/// OpenAI 兼容 chat completions 客户端（DeepSeek / OpenRouter）。
pub struct LlmClient {
    client: Client,
    base_url: String,
    model: String,
    prompt_mode: PromptMode,
    retry_delay: Duration,
}

impl LlmClient {
    pub fn new(
        api_key: &str,
        base_url: &str,
        model: &str,
        prompt_mode: PromptMode,
    ) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        let mut auth = header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| anyhow!("invalid LLM api key format"))?;
        auth.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, auth);
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .default_headers(headers)
            .build()
            .context("构建 LLM HTTP 客户端失败")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            prompt_mode,
            retry_delay: DEFAULT_RETRY_DELAY,
        })
    }

    /// 读取环境变量：优先 `DEEPSEEK_API_KEY`，否则 `OPENROUTER_API_KEY`；都没有返回 None。
    /// `LLM_PROMPT_MODE` 取 low（默认）/ high。
    pub fn from_env() -> Result<Option<Self>> {
        let non_empty = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let mode = PromptMode::parse(std::env::var("LLM_PROMPT_MODE").ok().as_deref());
        if let Some(key) = non_empty("DEEPSEEK_API_KEY") {
            return Self::new(&key, DEEPSEEK_BASE_URL, DEEPSEEK_DEFAULT_MODEL, mode).map(Some);
        }
        if let Some(key) = non_empty("OPENROUTER_API_KEY") {
            return Self::new(&key, OPENROUTER_BASE_URL, OPENROUTER_DEFAULT_MODEL, mode).map(Some);
        }
        Ok(None)
    }

    #[cfg(test)]
    pub fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    /// 发送聊天请求（最多 3 次尝试），返回首个 choice 的 content。
    async fn chat(&self, system: &str, user: String, max_tokens: u32) -> Result<Option<String>> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: system.to_string(),
                },
                ChatMessage {
                    role: "user",
                    content: user,
                },
            ],
            temperature: 0.0,
            max_tokens,
            stream: false,
        };

        let mut last_err = anyhow!("LLM request not attempted");
        for attempt in 1..=MAX_ATTEMPTS {
            match self.chat_once(&url, &body).await {
                Ok(resp) => {
                    let Some(choice) = resp.choices.into_iter().next() else {
                        return Ok(None);
                    };
                    if choice.finish_reason.as_deref() == Some("length") {
                        tracing::warn!(model = %resp.model, "LLM 输出被 max_tokens 截断，结果可能不可用");
                    }
                    return Ok(choice.message.and_then(|m| m.content));
                }
                Err(e) => {
                    tracing::warn!(attempt, max = MAX_ATTEMPTS, error = %e, "LLM 请求失败");
                    last_err = e;
                    if attempt < MAX_ATTEMPTS && !self.retry_delay.is_zero() {
                        tokio::time::sleep(self.retry_delay).await;
                    }
                }
            }
        }
        Err(last_err)
    }

    async fn chat_once(&self, url: &str, body: &ChatRequest) -> Result<ChatResponse> {
        let resp = self
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .context("发送 LLM 请求失败")?;
        let status = resp.status();
        if !status.is_success() {
            // 不回显响应体，避免意外带出敏感信息
            anyhow::bail!("LLM API 返回错误状态码: {status}");
        }
        resp.json::<ChatResponse>()
            .await
            .context("解析 LLM 响应失败")
    }

    pub(crate) fn build_match_input(&self, mal: &MalBrief, candidates: &[BgmCandidate]) -> String {
        let high = self.prompt_mode == PromptMode::High;
        let summary_max = if high {
            SUMMARY_MAX_CHARS_HIGH
        } else {
            SUMMARY_MAX_CHARS
        };

        let mut head = format!("MAL:{}", mal.title);
        if let Some(v) = non_empty(&mal.title_ja) {
            head.push_str(&format!("|{v}"));
        }
        if let Some(v) = non_empty(&mal.media_type) {
            head.push_str(&format!("|{v}"));
        }
        if high {
            if let Some(v) = non_empty(&mal.start_date) {
                head.push_str(&format!("|{v}"));
            }
            if let Some(n) = mal.num_episodes.filter(|n| *n > 0) {
                head.push_str(&format!("|{n}话"));
            }
            if let Some(v) = non_empty(&mal.source) {
                head.push_str(&format!("|{v}"));
            }
            if let Some(s) = mal.studios.as_ref().filter(|s| !s.is_empty()) {
                head.push_str(&format!("|{}", s.join("、")));
            }
            if let Some(v) = non_empty(&mal.synopsis) {
                head.push_str(&format!(
                    "|{}",
                    truncate_chars(&squash(v), SYNOPSIS_MAX_CHARS_HIGH)
                ));
            }
        }

        let mut lines = vec![head, "BGM:".to_string()];
        for c in candidates {
            let mut line = format!("{}:{}", c.bgm_id, c.name);
            if let Some(v) = non_empty(&c.name_cn) {
                line.push_str(&format!("|{v}"));
            }
            if let Some(v) = non_empty(&c.date) {
                line.push_str(&format!("|{v}"));
            }
            if high {
                if let Some(v) = non_empty(&c.platform) {
                    line.push_str(&format!("|{v}"));
                }
                if let Some(t) = c.tags.as_ref().filter(|t| !t.is_empty()) {
                    let tags: Vec<&str> =
                        t.iter().take(MAX_TAGS_HIGH).map(String::as_str).collect();
                    line.push_str(&format!("|{}", tags.join("、")));
                }
            }
            if let Some(v) = non_empty(&c.summary) {
                let text = if high { squash(v) } else { v.to_string() };
                line.push_str(&format!("|{}", truncate_chars(&text, summary_max)));
            }
            lines.push(line);
        }
        lines.join("\n")
    }

    pub async fn match_anime(
        &self,
        mal: &MalBrief,
        candidates: &[BgmCandidate],
    ) -> Result<Vec<LlmMatch>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let high = self.prompt_mode == PromptMode::High;
        let user = self.build_match_input(mal, candidates);
        let (system, max_tokens) = if high {
            (MATCH_SYSTEM_PROMPT_HIGH, MAX_TOKENS_MATCH_HIGH)
        } else {
            (MATCH_SYSTEM_PROMPT, MAX_TOKENS_MATCH)
        };
        let content = self
            .chat(system, user, max_tokens)
            .await?
            .ok_or_else(|| anyhow!("No response content"))?;
        Ok(parse_match_output(&content))
    }

    pub async fn suggest_search(&self, title_ja: &str, media_type: &str) -> Result<Suggest> {
        let user = format!("JA: {title_ja}\nType: {media_type}");
        let Some(content) = self
            .chat(SUGGEST_SYSTEM_PROMPT, user, MAX_TOKENS_SUGGEST)
            .await?
        else {
            return Ok(Suggest::Keywords(Vec::new()));
        };
        Ok(parse_suggest_output(&content))
    }
}

impl LlmMatcher for LlmClient {
    fn match_anime<'a>(
        &'a self,
        mal: &'a MalBrief,
        candidates: &'a [BgmCandidate],
    ) -> BoxFuture<'a, Result<Vec<LlmMatch>>> {
        Box::pin(LlmClient::match_anime(self, mal, candidates))
    }

    fn suggest_search<'a>(
        &'a self,
        title_ja: &'a str,
        media_type: &'a str,
    ) -> BoxFuture<'a, Result<Suggest>> {
        Box::pin(LlmClient::suggest_search(self, title_ja, media_type))
    }
}

fn non_empty(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| !s.is_empty())
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// 从 LLM 响应中提取 JSON：处理纯 JSON、```json 围栏、缺失收尾围栏（输出被截断）。
pub fn extract_json(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.starts_with("```")
        && let Some(nl) = trimmed.find('\n')
    {
        let after = &trimmed[nl + 1..];
        return match after.rfind("```") {
            Some(end) => after[..end].trim().to_string(),
            None => after.trim().to_string(),
        };
    }
    trimmed.to_string()
}

fn parse_match_output(content: &str) -> Vec<LlmMatch> {
    let json = extract_json(content);
    if json.is_empty() {
        tracing::warn!("match_anime: 模型返回空响应");
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else {
        tracing::warn!("match_anime: 无效 JSON");
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let bgm_id = item.get("bgm_id").and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })?;
            let bgm_id = i32::try_from(bgm_id).ok()?;
            let confidence = item.get("confidence").and_then(|v| v.as_f64());
            let reason = item
                .get("reason")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| truncate_chars(s, REASON_MAX_CHARS));
            Some(LlmMatch {
                bgm_id,
                confidence,
                reason,
            })
        })
        .collect()
}

fn parse_suggest_output(content: &str) -> Suggest {
    let json = extract_json(content);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else {
        tracing::warn!("suggest_search: 无效 JSON");
        return Suggest::Keywords(Vec::new());
    };
    if value.get("skip").and_then(|v| v.as_bool()) == Some(true) {
        return Suggest::Skip;
    }
    let keywords = value
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Suggest::Keywords(keywords)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    fn client(url: &str, mode: PromptMode) -> LlmClient {
        LlmClient::new("test-key", url, "test-model", mode)
            .unwrap()
            .with_retry_delay(Duration::ZERO)
    }

    fn chat_body(content: &str) -> String {
        serde_json::json!({
            "model": "test-model",
            "choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]
        })
        .to_string()
    }

    #[test]
    fn test_extract_json() {
        assert_eq!(extract_json("  [1] "), "[1]");
        assert_eq!(extract_json("```json\n[1]\n```"), "[1]");
        assert_eq!(extract_json("```\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(extract_json("```json\n[1]"), "[1]");
    }

    #[test]
    fn test_prompt_mode_parse() {
        assert_eq!(PromptMode::parse(None), PromptMode::Low);
        assert_eq!(PromptMode::parse(Some(" HIGH ")), PromptMode::High);
        assert_eq!(PromptMode::parse(Some("weird")), PromptMode::Low);
    }

    #[test]
    fn test_parse_outputs() {
        let m = parse_match_output(
            r#"[{"bgm_id":1,"confidence":0.9,"reason":"ok"},{"confidence":1},{"bgm_id":"2"}]"#,
        );
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].confidence, Some(0.9));
        assert_eq!(m[1].bgm_id, 2);
        assert!(parse_match_output("not json").is_empty());
        assert_eq!(parse_suggest_output(r#"{"skip":true}"#), Suggest::Skip);
        assert_eq!(
            parse_suggest_output("```json\n{\"keywords\":[\"a\",\" \"]}\n```"),
            Suggest::Keywords(vec!["a".into()])
        );
        assert_eq!(parse_suggest_output("x"), Suggest::Keywords(vec![]));
    }

    #[test]
    fn test_build_match_input_low_and_high() {
        let mal = MalBrief {
            title: "Frieren".into(),
            title_ja: Some("葬送のフリーレン".into()),
            media_type: Some("tv".into()),
            start_date: Some("2023-09-29".into()),
            num_episodes: Some(28),
            ..Default::default()
        };
        let cands = [BgmCandidate {
            bgm_id: 1,
            name: "葬送のフリーレン".into(),
            date: Some("2023-09-29".into()),
            platform: Some("TV".into()),
            ..Default::default()
        }];
        let low = client("http://x", PromptMode::Low).build_match_input(&mal, &cands);
        assert_eq!(
            low,
            "MAL:Frieren|葬送のフリーレン|tv\nBGM:\n1:葬送のフリーレン|2023-09-29"
        );
        let high = client("http://x", PromptMode::High).build_match_input(&mal, &cands);
        assert!(high.contains("|2023-09-29|28话"));
        assert!(high.ends_with("1:葬送のフリーレン|2023-09-29|TV"));
    }

    #[tokio::test]
    async fn test_match_anime_request_and_response() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .match_header("authorization", "Bearer test-key")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "model": "test-model",
                "max_tokens": 2048,
                "temperature": 0.0
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(chat_body(
                "```json\n[{\"bgm_id\":7,\"confidence\":0.93}]\n```",
            ))
            .create_async()
            .await;

        let c = client(&server.url(), PromptMode::Low);
        let mal = MalBrief {
            title: "t".into(),
            ..Default::default()
        };
        let cands = [BgmCandidate {
            bgm_id: 7,
            name: "n".into(),
            ..Default::default()
        }];
        let out = c.match_anime(&mal, &cands).await.unwrap();
        mock.assert_async().await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].bgm_id, 7);
        assert_eq!(out[0].confidence, Some(0.93));
    }

    #[tokio::test]
    async fn test_high_mode_uses_larger_max_tokens() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::PartialJson(
                serde_json::json!({"max_tokens": 3072}),
            ))
            .with_status(200)
            .with_body(chat_body("[]"))
            .create_async()
            .await;
        let c = client(&server.url(), PromptMode::High);
        let mal = MalBrief::default();
        let cands = [BgmCandidate::default()];
        assert!(c.match_anime(&mal, &cands).await.unwrap().is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_suggest_search_and_retry() {
        let mut server = mockito::Server::new_async().await;
        let fail = server
            .mock("POST", "/chat/completions")
            .with_status(500)
            .expect(2)
            .create_async()
            .await;
        let ok = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::PartialJson(
                serde_json::json!({"max_tokens": 2048}),
            ))
            .with_status(200)
            .with_body(chat_body(r#"{"keywords":["銀魂"]}"#))
            .create_async()
            .await;
        let c = client(&server.url(), PromptMode::Low);
        let out = c.suggest_search("新劇場版 銀魂", "movie").await.unwrap();
        assert_eq!(out, Suggest::Keywords(vec!["銀魂".into()]));
        fail.assert_async().await;
        ok.assert_async().await;
    }

    #[tokio::test]
    async fn test_gives_up_after_three_attempts() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", "/chat/completions")
            .with_status(503)
            .expect(3)
            .create_async()
            .await;
        let c = client(&server.url(), PromptMode::Low);
        assert!(c.suggest_search("x", "tv").await.is_err());
        m.assert_async().await;
    }
}
