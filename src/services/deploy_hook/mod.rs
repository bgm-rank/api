pub struct DeployHookClient {
    client: reqwest::Client,
    hook_url: Option<String>,
}

impl DeployHookClient {
    pub fn new(hook_url: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            hook_url,
        }
    }

    pub async fn trigger(&self) -> anyhow::Result<()> {
        let Some(url) = &self.hook_url else {
            tracing::warn!("DEPLOY_HOOK_URL 未配置，跳过触发");
            return Ok(());
        };
        let resp = self.client.post(url).send().await?;
        tracing::info!(status = %resp.status(), "Deploy Hook 已触发");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // T007 🔴 → 🟢
    #[tokio::test]
    async fn test_trigger_skips_when_url_none() {
        let client = DeployHookClient::new(None);
        let result = client.trigger().await;
        assert!(result.is_ok());
    }

    // T008 🔴 → 🟢
    #[tokio::test]
    async fn test_trigger_posts_to_url() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .create_async()
            .await;

        let client = DeployHookClient::new(Some(server.url()));
        let result = client.trigger().await;
        assert!(result.is_ok());
        mock.assert_async().await;
    }
}
