use crate::services::bangumi::{BangumiClient, schemas::*};
use anyhow::{Context, Result};

impl BangumiClient {
    pub async fn get_subject(&self, subject_id: i32) -> Result<Subject> {
        /*
            获取条目详细信息

        GET /v0/subjects/{subject_id}

        Args:
            subject_id: 条目ID

        Returns:
            Subject: 条目详细信息
        */
        let url = self
            .base_url
            .join(&format!("/v0/subjects/{}", subject_id))
            .unwrap();

        let start = std::time::Instant::now();
        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .inspect_err(|e| {
                // T019: network-level failure
                tracing::warn!(url = %url, subject_id, error = %e, "bangumi api call failed");
            })
            .context("发送请求失败")?;

        let elapsed_ms = start.elapsed().as_millis() as u64;
        let status_code = response.status().as_u16();

        if !response.status().is_success() {
            // T019: non-2xx response
            tracing::warn!(url = %url, subject_id, elapsed_ms, status_code, "bangumi api call failed");
            anyhow::bail!("API 返回错误状态码: {}, URL: {}", response.status(), url);
        }

        // T018: successful API call
        tracing::debug!(url = %url, subject_id, elapsed_ms, status_code, "bangumi api call");

        let subject = response
            .json::<Subject>()
            .await
            .context("解析响应 JSON 失败")?;

        Ok(subject)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_subject() {
        let client = BangumiClient::new();
        let subject = client.get_subject(400602).await.unwrap();

        assert_eq!(subject.id, 400602);
        assert_eq!(subject.name_cn, Some("葬送的芙莉莲".to_string()));
        assert!(subject.rating.is_some());
    }

    #[tokio::test]
    async fn test_bad_subject_not_found() {
        let client = BangumiClient::new();
        let subject = client.get_subject(999999999).await;

        assert!(subject.is_err());
    }

    // T014 [US2]: 模拟 Bangumi API 失败，验证 WARN 事件包含 error 和 subject_id 字段
    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_api_call_failure_logs_warn_with_subject_id_and_error() {
        let client = BangumiClient::new();
        let _ = client.get_subject(999999999).await;
        assert!(logs_contain("subject_id"), "API 失败日志应包含 subject_id 字段");
        assert!(
            logs_contain("bangumi api call failed"),
            "应记录 bangumi api call failed WARN 日志"
        );
    }

    #[tokio::test]
    async fn test_get_subject_redirect() {
        let client = BangumiClient::new();
        let subject = client.get_subject(141079).await.unwrap();

        assert_eq!(subject.id, 104906);
        assert_eq!(subject.name_cn, Some("境界触发者".to_string()));
        assert!(subject.rating.is_some());
    }
}
