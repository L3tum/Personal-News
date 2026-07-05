use crate::config::FreshRSSConfig;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::cmp::Reverse;

#[derive(Debug, Deserialize, Clone)]
pub struct Article {
    pub id: u64,
    pub title: String,
    pub author: Option<String>,
    pub content: String, // HTML content
    pub url: String,
    pub feed_title: String,
    pub feed_url: String,
    pub language: Option<String>, // ISO 639-1 language code from the RSS feed, if available
    pub published: i64,           // Unix timestamp
}

pub struct FreshRSSClient {
    client: Client,
    config: FreshRSSConfig,
}

impl FreshRSSClient {
    pub fn new(config: FreshRSSConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("Failed to create HTTP client"),
            config,
        }
    }

    /// Fetch unread articles for a specific user, limited to the last 24 hours.
    pub async fn fetch_unread_articles(
        &self,
        user: &str,
        since: i64,
    ) -> anyhow::Result<Vec<Article>> {
        let encoded_user = urlencoding::encode(user);
        let url = format!("{}/api/g.php?get=entries&user={}&feeds=-1&state=_notread&since={}&order=desc&sort=date&export=flatjson",
            self.config.url, encoded_user, since);

        let response = self
            .client
            .get(&url)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("FreshRSS API error: {} - {}", status, body));
        }

        let entries: Value = response.json().await?;

        // The response is an object where keys are article IDs
        let articles = entries
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Unexpected FreshRSS response format"))?;

        let mut article_vec = Vec::new();
        for (id_str, entry) in articles {
            let id: u64 = id_str.parse()?;
            let title = entry
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = entry
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = entry
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let feed_title = entry
                .get("feed")
                .and_then(|v| v.as_object())
                .and_then(|f| f.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown Feed")
                .to_string();
            let feed_url = entry
                .get("feed")
                .and_then(|v| v.as_object())
                .and_then(|f| f.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let published = entry.get("date").and_then(|v| v.as_i64()).unwrap_or(0);
            let author = entry
                .get("author")
                .and_then(|v| v.as_str())
                .map(String::from);
            // The language field is the RSS feed's primary language (ISO 639-1) if available
            let language = entry
                .get("language")
                .and_then(|v| v.as_str())
                .map(String::from);

            article_vec.push(Article {
                id,
                title,
                author,
                content,
                url,
                feed_title,
                feed_url,
                language,
                published,
            });
        }

        // Sort by date descending
        article_vec.sort_by_key(|a| Reverse(a.published));

        Ok(article_vec)
    }

    /// Mark articles as read (POST to avoid URL length limits)
    pub async fn mark_as_read(&self, user: &str, article_ids: &[u64]) -> anyhow::Result<()> {
        let body = format!(
            "get=markEntriesAsRead&user={}&entries={}",
            user,
            article_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        let url = format!("{}/api/g.php", self.config.url);
        let response = self
            .client
            .post(&url)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to mark articles as read: {}",
                response.status()
            ));
        }

        Ok(())
    }
}
