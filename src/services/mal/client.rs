use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode, header};
use std::time::Duration;

use super::schemas::{MalAnimeNode, MalSeasonalResponse};

const MAL_API_BASE: &str = "https://api.myanimelist.net/v2";

pub const FIELDS: &str = "id,title,alternative_titles,start_date,end_date,synopsis,\
media_type,status,num_episodes,start_season,broadcast,source,studios,rating";

const MAX_RETRIES: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(5);
const REQUEST_INTERVAL: Duration = Duration::from_millis(500);
const PAGE_LIMIT: u32 = 500;

pub struct MalClient {
    client: Client,
    base_url: String,
    retry_delay: Duration,
    request_interval: Duration,
}

impl MalClient {
    /// 从环境变量 `MAL_CLIENT_ID` 读取凭据构造客户端（不记录其值）。
    pub fn from_env() -> Result<Self> {
        let client_id = std::env::var("MAL_CLIENT_ID").unwrap_or_default();
        if client_id.trim().is_empty() {
            bail!("MAL_CLIENT_ID environment variable is required");
        }
        Self::new(&client_id)
    }

    pub fn new(client_id: &str) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        let mut value = header::HeaderValue::from_str(client_id)
            .context("MAL_CLIENT_ID contains invalid header characters")?;
        value.set_sensitive(true);
        headers.insert("X-MAL-CLIENT-ID", value);

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self {
            client,
            base_url: MAL_API_BASE.to_string(),
            retry_delay: RETRY_DELAY,
            request_interval: REQUEST_INTERVAL,
        })
    }

    /// 指向自定义 base url 并缩短重试/翻页间隔，供测试注入 mock server
    #[cfg(test)]
    pub fn with_base_url(
        base_url: &str,
        retry_delay: Duration,
        request_interval: Duration,
    ) -> Self {
        let mut c = Self::new("test-client-id").unwrap();
        c.base_url = base_url.trim_end_matches('/').to_string();
        c.retry_delay = retry_delay;
        c.request_interval = request_interval;
        c
    }

    pub fn request_interval(&self) -> Duration {
        self.request_interval
    }

    async fn get(&self, url: &str, params: &[(&str, String)]) -> Result<reqwest::Response> {
        let mut last_status = None;
        for attempt in 1..=MAX_RETRIES {
            let resp = self.client.get(url).query(params).send().await?;
            let status = resp.status();
            if matches!(status.as_u16(), 307 | 429 | 500 | 502 | 503) {
                last_status = Some(status);
                if attempt < MAX_RETRIES {
                    let delay = self.retry_delay * attempt;
                    tracing::warn!(
                        status = status.as_u16(),
                        attempt,
                        max_retries = MAX_RETRIES,
                        delay_ms = delay.as_millis() as u64,
                        "MAL request failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
                continue;
            }
            if !status.is_success() {
                bail!("MAL request failed with status {}", status);
            }
            return Ok(resp);
        }
        let status = last_status.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        bail!(
            "MAL request failed after {} attempts: {}",
            MAX_RETRIES,
            status
        )
    }

    /// 获取指定季度新番列表（单页）。`season` 为小写 winter/spring/summer/fall。
    pub async fn get_seasonal_anime(
        &self,
        year: i32,
        season: &str,
        limit: u32,
        offset: u32,
        nsfw: bool,
    ) -> Result<MalSeasonalResponse> {
        let url = format!("{}/anime/season/{}/{}", self.base_url, year, season);
        let mut params = vec![
            ("fields", FIELDS.to_string()),
            ("limit", limit.min(PAGE_LIMIT).to_string()),
            ("offset", offset.to_string()),
        ];
        if nsfw {
            params.push(("nsfw", "true".to_string()));
        }
        let resp = self.get(&url, &params).await?;
        Ok(resp.json::<MalSeasonalResponse>().await?)
    }

    /// 获取指定季度的所有新番（自动分页）。
    pub async fn get_all_seasonal_anime(
        &self,
        year: i32,
        season: &str,
    ) -> Result<Vec<MalAnimeNode>> {
        let mut all = Vec::new();
        let mut offset = 0u32;
        loop {
            let resp = self
                .get_seasonal_anime(year, season, PAGE_LIMIT, offset, true)
                .await?;
            let count = resp.data.len() as u32;
            all.extend(resp.data.into_iter().map(|e| e.node));

            let has_next = resp.paging.next.as_deref().is_some_and(|n| !n.is_empty());
            if !has_next || count < PAGE_LIMIT {
                break;
            }
            offset += PAGE_LIMIT;
            tokio::time::sleep(self.request_interval).await;
        }
        Ok(all)
    }
}
