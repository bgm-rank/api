use anyhow::{Context, Result};
use reqwest::{Client, Url, header};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

const SEASON_DATA_URL: &str =
    "https://github.com/bgm-rank/season-data/releases/latest/download/season-data.json";
const USER_AGENT: &str = "rinshankaiho.fun (https://github.com/hexsix/bgm-rank-api)";

#[derive(Debug, Deserialize)]
pub struct SeasonEntry {
    pub bgm_id: i32,
    pub media_type: MediaType,
    pub rating: Rating,
}

pub type SeasonData = HashMap<String, Vec<SeasonEntry>>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Tv,
    Movie,
    Ova,
    Ona,
    TvSpecial,
    Special,
    Music,
    Pv,
    Cm,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rating {
    General,
    Kids,
    R18,
}

pub struct SeasonDataClient {
    client: reqwest::Client,
    url: Arc<Url>,
}

impl SeasonDataClient {
    pub fn new() -> Self {
        Self::with_url(SEASON_DATA_URL)
    }

    pub fn with_url(url: impl Into<String>) -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(USER_AGENT),
        );

        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .default_headers(headers)
            .build()
            .unwrap();

        Self {
            client,
            url: Arc::from(reqwest::Url::parse(&url.into()).unwrap()),
        }
    }

    // 获取全部季度数据
    pub async fn fetch_all(&self) -> Result<SeasonData> {
        let response = self
            .client
            .get(self.url.as_str())
            .send()
            .await
            .context("发送请求失败")?;

        if !response.status().is_success() {
            anyhow::bail!(
                "API 返回错误状态码: {}, URL: {}",
                response.status(),
                self.url
            );
        }

        let season_data = response
            .json::<SeasonData>()
            .await
            .context("解析响应 JSON 失败")?;

        Ok(season_data)
    }

    // 获取某一季度的番剧列表，如 "2026-winter"
    pub async fn fetch_season(&self, key: &str) -> anyhow::Result<Vec<SeasonEntry>> {
        let mut all = self.fetch_all().await?;
        match all.remove(key) {
            Some(entries) => Ok(entries),
            None => {
                let available_keys: Vec<&str> = all.keys().map(|k| k.as_str()).collect();
                tracing::warn!(
                    key,
                    available_keys = ?available_keys,
                    "season key not found in season-data.json"
                );
                Ok(vec![])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_all_parses_correctly() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/season-data.json")
            .with_body(
                r#"{"2026-winter": [{"bgm_id": 515759, "media_type": "tv", "rating": "general"}]}"#,
            )
            .create_async()
            .await;

        let client = SeasonDataClient::with_url(server.url() + "/season-data.json");
        let data = client.fetch_all().await.unwrap();

        assert_eq!(data["2026-winter"].len(), 1);
        assert_eq!(data["2026-winter"][0].bgm_id, 515759);
        mock.assert_async().await;
    }
}
