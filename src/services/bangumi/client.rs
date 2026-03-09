use reqwest::{Client, Url, header};
use std::sync::Arc;

const BANGUMI_API_BASE: &str = "https://api.bgm.tv";
const USER_AGENT: &str = "rinshankaiho.fun (https://github.com/hexsix/bgm-rank-api)";

pub struct BangumiClient {
    pub(super) client: Client,
    pub(super) base_url: Arc<Url>,
}

impl BangumiClient {
    pub fn new() -> Self {
        let token = std::env::var("BGM_TOKEN").ok();

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(USER_AGENT),
        );
        if let Some(token) = token {
            let auth_value = header::HeaderValue::from_str(&format!("Bearer {}", token));
            if let Ok(auth_value) = auth_value {
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
        }
    }
}

impl Default for BangumiClient {
    fn default() -> Self {
        Self::new()
    }
}
