use crate::services::bangumi::{BangumiClient, schemas::*};
use anyhow::{Context, Result};

const SEARCH_MAX_ATTEMPTS: u32 = 3;

impl BangumiClient {
    /// 搜索条目（对瞬时 429 / 5xx 错误自动退避重试）
    ///
    /// POST /v0/search/subjects?limit=&offset=
    pub async fn search_subjects(
        &self,
        request: &SearchRequest,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> Result<PagedSubject> {
        let url = self.base_url.join("/v0/search/subjects").unwrap();

        let mut query: Vec<(&str, i32)> = Vec::new();
        if let Some(l) = limit {
            query.push(("limit", l));
        }
        if let Some(o) = offset {
            query.push(("offset", o));
        }

        for attempt in 1..=SEARCH_MAX_ATTEMPTS {
            let start = std::time::Instant::now();
            let send_res = self
                .client
                .post(url.as_str())
                .query(&query)
                .json(request)
                .send()
                .await;

            let response = match send_res {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(url = %url, attempt, error = %e, "bangumi search failed");
                    if attempt < SEARCH_MAX_ATTEMPTS {
                        if !self.retry_delay.is_zero() {
                            tokio::time::sleep(self.retry_delay * attempt).await;
                        }
                        continue;
                    }
                    return Err(e).context("发送搜索请求失败");
                }
            };

            let elapsed_ms = start.elapsed().as_millis() as u64;
            let status = response.status();
            let status_code = status.as_u16();
            if matches!(status_code, 429 | 500 | 502 | 503 | 504) && attempt < SEARCH_MAX_ATTEMPTS {
                tracing::warn!(
                    url = %url,
                    elapsed_ms,
                    status_code,
                    attempt,
                    "bangumi search transient error, retrying"
                );
                if !self.retry_delay.is_zero() {
                    tokio::time::sleep(self.retry_delay * attempt).await;
                }
                continue;
            }
            if !status.is_success() {
                tracing::warn!(url = %url, elapsed_ms, status_code, "bangumi search failed");
                anyhow::bail!("搜索 API 返回错误状态码: {}", status);
            }
            tracing::debug!(url = %url, elapsed_ms, status_code, "bangumi search");

            return response
                .json::<PagedSubject>()
                .await
                .context("解析搜索响应 JSON 失败");
        }

        unreachable!()
    }

    /// 按关键词搜索动画（含 NSFW），`air_date_range` 为 `(start, end)`：`>=start` 且 `<end`。
    pub async fn search_anime(
        &self,
        keyword: &str,
        air_date_range: Option<(&str, &str)>,
    ) -> Result<Vec<Subject>> {
        let mut filter = SearchFilter::from_type(2);
        filter.nsfw = Some(true);
        if let Some((start, end)) = air_date_range {
            filter.air_date = Some(vec![format!(">={start}"), format!("<{end}")]);
        }
        let request = SearchRequest::new(keyword).with_filter(filter);
        Ok(self.search_subjects(&request, Some(10), None).await?.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    #[tokio::test]
    async fn test_search_subjects_sends_filter_and_parses() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::UrlEncoded("limit".into(), "10".into()))
            .match_body(Matcher::PartialJson(serde_json::json!({
                "keyword": "葬送のフリーレン",
                "sort": "match",
                "filter": {
                    "type": [2],
                    "air_date": [">=2023-09-01", "<2023-12-31"],
                    "nsfw": true
                }
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"total":1,"limit":10,"offset":0,"data":[{"id":400602,"type":2,"name":"葬送のフリーレン","name_cn":"葬送的芙莉莲","date":"2023-09-29"}]}"#,
            )
            .create_async()
            .await;

        let client = BangumiClient::with_base_url(&server.url());
        let found = client
            .search_anime("葬送のフリーレン", Some(("2023-09-01", "2023-12-31")))
            .await
            .unwrap();
        mock.assert_async().await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, 400602);
        assert_eq!(found[0].date.as_deref(), Some("2023-09-29"));
    }

    #[tokio::test]
    async fn test_search_without_range_omits_air_date() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::Any)
            .match_body(Matcher::PartialJson(serde_json::json!({
                "filter": {"type": [2], "nsfw": true}
            })))
            .match_request(|r| !String::from_utf8_lossy(r.body().unwrap()).contains("air_date"))
            .with_status(200)
            .with_body(r#"{"total":0,"limit":10,"offset":0,"data":[]}"#)
            .create_async()
            .await;

        let client = BangumiClient::with_base_url(&server.url());
        let found = client.search_anime("x", None).await.unwrap();
        mock.assert_async().await;
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn test_search_retries_on_transient_status_then_succeeds() {
        let mut server = mockito::Server::new_async().await;
        let fail = server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::Any)
            .with_status(429)
            .expect(2)
            .create_async()
            .await;
        let ok = server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"total":1,"limit":10,"offset":0,"data":[{"id":400602,"type":2,"name":"葬送のフリーレン"}]}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let client = BangumiClient::with_base_url(&server.url());
        let found = client.search_anime("葬送のフリーレン", None).await.unwrap();
        fail.assert_async().await;
        ok.assert_async().await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, 400602);
    }

    #[tokio::test]
    async fn test_search_error_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::Any)
            .with_status(500)
            .expect(3)
            .create_async()
            .await;
        let client = BangumiClient::with_base_url(&server.url());
        assert!(client.search_anime("x", None).await.is_err());
        mock.assert_async().await;
    }
}
