use reqwest::{Client, Url, header};
use std::sync::Arc;
use std::time::Duration;

const BANGUMI_API_BASE: &str = "https://api.bgm.tv";
const USER_AGENT: &str = "rinshankaiho.fun (https://github.com/hexsix/bgm-rank-api)";
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(1);

pub struct BangumiClient {
    pub(super) client: Client,
    pub(super) base_url: Arc<Url>,
    pub(super) retry_delay: Duration,
}

impl BangumiClient {
    pub fn new() -> Self {
        let token = std::env::var("BGM_TOKEN").ok();

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(USER_AGENT),
        );
        if let Some(token) = token.filter(|t| !t.trim().is_empty()) {
            let auth_value = header::HeaderValue::from_str(&format!("Bearer {}", token));
            if let Ok(mut auth_value) = auth_value {
                auth_value.set_sensitive(true);
                headers.insert(header::AUTHORIZATION, auth_value);
            }
        }

        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .default_headers(headers)
            .build()
            .unwrap();

        Self {
            client,
            base_url: Arc::from(reqwest::Url::parse(BANGUMI_API_BASE).unwrap()),
            retry_delay: DEFAULT_RETRY_DELAY,
        }
    }

    /// 指向自定义 base url，供测试注入 mock server
    #[cfg(test)]
    pub fn with_base_url(base_url: &str) -> Self {
        let mut client = Self::new();
        client.base_url = Arc::from(reqwest::Url::parse(base_url).unwrap());
        client.retry_delay = Duration::ZERO;
        client
    }
}

impl Default for BangumiClient {
    fn default() -> Self {
        Self::new()
    }
}
