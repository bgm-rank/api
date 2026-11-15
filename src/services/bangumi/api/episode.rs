use crate::services::bangumi::{BangumiClient, schemas::*};
use anyhow::{Context, Result};

#[allow(dead_code)]
impl BangumiClient {
    pub async fn get_episodes(
        &self,
        subject_id: i32,
        episode_type: i32,
        limit: i32,
        offset: i32,
    ) -> Result<PagedEpisode> {
        /*
            获取剧集信息

        GET /v0/episodes

        Args:
            subject_id: 条目ID
            episode_type: 剧集类型 (本篇=0 特别篇=1 OP=2 ED=3 预告/宣传/广告=4 MAD=5 其他=6)
            limit: 每页数量
            offset: 偏移量

        Returns:
            PagedEpisode: 分页剧集数据
        */
        let url = self.base_url.join("/v0/episodes").unwrap();

        let response = self
            .client
            .get(url.as_str())
            .query(&[
                ("subject_id", &subject_id.to_string()),
                ("type", &episode_type.to_string()),
                ("limit", &limit.to_string()),
                ("offset", &offset.to_string()),
            ])
            .send()
            .await
            .inspect_err(|e| {
                tracing::warn!(url = %url, subject_id, error = %e, "bangumi api call failed");
            })
            .context("发送请求失败")?;

        if !response.status().is_success() {
            anyhow::bail!("API 返回错误状态码: {}, URL: {}", response.status(), url);
        }

        let paged_episodes = response
            .json::<PagedEpisode>()
            .await
            .context("解析响应 JSON 失败")?;

        Ok(paged_episodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    #[tokio::test]
    async fn test_get_episodes() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v0/episodes")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("subject_id".into(), "400602".into()),
                Matcher::UrlEncoded("type".into(), "0".into()),
                Matcher::UrlEncoded("limit".into(), "100".into()),
                Matcher::UrlEncoded("offset".into(), "0".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "total": 2,
                    "limit": 100,
                    "offset": 0,
                    "data": [
                        {"id": 1227087, "type": 0, "ep": 1.0, "airdate": "2023-09-29", "comment": 120},
                        {"id": 1227088, "type": 0, "ep": 2.0, "airdate": "2023-09-29", "comment": 95}
                    ]
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = BangumiClient::with_base_url(&server.url());
        let paged_episode = client.get_episodes(400602, 0, 100, 0).await.unwrap();
        mock.assert_async().await;
        assert_eq!(paged_episode.total, 2);
        assert_eq!(paged_episode.data.len(), 2);
        assert_eq!(
            paged_episode.data[0].airdate,
            Some("2023-09-29".to_string())
        );
        assert_eq!(paged_episode.data[0].id, 1227087);
    }

    #[tokio::test]
    async fn test_bad_episodes_not_found() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v0/episodes")
            .match_query(Matcher::Any)
            .with_status(404)
            .create_async()
            .await;

        let client = BangumiClient::with_base_url(&server.url());
        let paged_episode = client.get_episodes(999999999, 0, 100, 0).await;
        mock.assert_async().await;
        assert!(paged_episode.is_err());
    }
}
