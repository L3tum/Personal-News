//! FreshRSS Fever API client - documented and reliable
//!
//! The Fever API is FreshRSS's recommended public API, well-documented and
//! returning proper JSON. It uses MD5-based authentication with an API password
//! (the regular password should work in most cases).

use crate::config::FreshRSSConfig;
use reqwest::Client;
use serde::Deserialize;
use std::cmp::Reverse;

/// Article as returned by the Fever API
#[derive(Debug, Deserialize, Clone)]
pub struct Article {
    #[serde(rename = "id")]
    pub id: u64,
    #[serde(rename = "title")]
    pub title: String,
    #[serde(rename = "author")]
    pub author: Option<String>,
    #[serde(rename = "body")]
    pub content: String, // HTML content
    #[serde(rename = "url")]
    pub url: String,
    #[serde(rename = "feed_title")]
    pub feed_title: String,
    #[serde(rename = "feed_url")]
    pub feed_url: Option<String>,
    #[serde(rename = "created")]
    pub published: i64, // Unix timestamp
    // Fever API doesn't provide a language field
    pub language: Option<String>,
}

/// Intermediate response structure for Fever API items endpoint
#[derive(Debug, Deserialize)]
struct FeverItemsResponse {
    items: Vec<FeverItem>,
}

#[derive(Debug, Deserialize, Clone)]
struct FeverItem {
    #[serde(rename = "id")]
    id: u64,
    #[serde(rename = "title")]
    title: String,
    #[serde(rename = "author")]
    author: Option<String>,
    #[serde(rename = "body")]
    body: String,
    #[serde(rename = "url")]
    url: String,
    #[serde(rename = "feed_title")]
    feed_title: String,
    #[serde(rename = "feed_url")]
    feed_url: Option<String>,
    #[serde(rename = "created")]
    created: i64,
}

/// Authentication response from the Fever API
#[derive(Debug, Deserialize)]
struct FeverAuthResponse {
    #[serde(rename = "api_version")]
    #[allow(dead_code)] // we only care about the `auth` field
    api_version: i32,
    #[serde(rename = "auth")]
    auth: i32,
}

pub struct FeverClient {
    client: Client,
    config: FreshRSSConfig,
}

impl FeverClient {
    pub fn new(config: FreshRSSConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("Failed to create HTTP client"),
            config,
        }
    }

    /// Compute the Fever API key: MD5("username:apiPassword")
    /// Uses the regular password if no API password is provided.
    fn api_key(&self, username: &str, password: &str) -> String {
        let data = format!("{}:{}", username, password);
        let md5 = md5::compute(&data);
        format!("{:x}", md5)
    }

    /// Fetch unread articles for a specific user since the given timestamp.
    pub async fn fetch_unread_articles(
        &self,
        username: &str,
        password: Option<&str>,
        since: i64,
    ) -> anyhow::Result<Vec<Article>> {
        let url = format!("{}/api/fever.php", self.config.url);
        let password_to_use = password.unwrap_or(&self.config.password);
        let api_key = self.api_key(username, password_to_use);

        // First, verify authentication works
        let auth_response: FeverAuthResponse = self
            .client
            .post(format!("{}?api", url))
            .form(&[("api_key", &api_key)])
            .send()
            .await?
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Fever auth response: {}", e))?;

        if auth_response.auth != 1 {
            return Err(anyhow::anyhow!(
                "Fever API authentication failed for user {}. Check that you have an API password set (Settings > API password), or use the regular password if no API password is configured.",
                username
            ));
        }

        // Fetch unread items with full details
        let items_response: FeverItemsResponse = self
            .client
            .post(format!("{}?api&unread_item_ids", url))
            .form(&[("api_key", &api_key)])
            .send()
            .await?
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Fever items response: {}", e))?;

        let mut article_vec = Vec::new();
        for item in items_response.items {
            // Filter by since timestamp (last 24 hours)
            if item.created < since {
                continue;
            }

            article_vec.push(Article {
                id: item.id,
                title: item.title,
                author: item.author,
                content: item.body,
                url: item.url,
                feed_title: item.feed_title,
                feed_url: item.feed_url,
                published: item.created,
                language: None, // Fever API doesn't provide language
            });
        }

        // Sort by date descending
        article_vec.sort_by_key(|a| Reverse(a.published));

        log::debug!(
            "Fever API returned {} unread articles for user {} since {}",
            article_vec.len(),
            username,
            chrono::DateTime::from_timestamp(since, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or("unknown".to_string())
        );

        Ok(article_vec)
    }

    /// Mark articles as read using the Fever API.
    pub async fn mark_as_read(
        &self,
        username: &str,
        password: Option<&str>,
        article_ids: &[u64],
    ) -> anyhow::Result<()> {
        let url = format!("{}/api/fever.php", self.config.url);
        let password_to_use = password.unwrap_or(&self.config.password);
        let api_key = self.api_key(username, password_to_use);

        // Mark each article as read individually
        for id in article_ids {
            let response = self
                .client
                .post(format!("{}?api&mark=item&as=read&id={}", url, id))
                .form(&[("api_key", &api_key)])
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!(
                    "Failed to mark article {} as read: {} - {}",
                    id,
                    status,
                    body
                ));
            }
        }

        Ok(())
    }
}
