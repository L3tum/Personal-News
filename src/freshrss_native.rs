//! Native FreshRSS API client (entries.json) - more reliable than greader.php

use crate::config::FreshRSSConfig;
use reqwest::Client;
use serde::Deserialize;
use std::cmp::Reverse;

#[derive(Debug, Deserialize, Clone)]
pub struct Article {
    pub id: u64,
    pub title: String,
    #[serde(rename = "author")]
    pub author: Option<String>,
    #[serde(rename = "body")]
    pub content: String, // HTML content (renamed for consistency with digest.rs)
    #[serde(rename = "link")]
    pub url: String,
    #[serde(rename = "feed_title")]
    pub feed_title: Option<String>,
    #[serde(rename = "feed_url")]
    pub feed_url: Option<String>,
    #[serde(rename = "language")]
    pub language: Option<String>, // from the RSS feed, if available
    #[serde(rename = "updated")]
    pub published: i64, // Unix timestamp
}

/// Intermediate deserializer since FreshRSS wraps feed info in a "feed" object
#[derive(Debug, Deserialize, Clone)]
struct FreshRSSArticle {
    pub id: u64,
    pub title: String,
    pub author: Option<String>,
    pub body: String,
    pub link: String,
    pub feed: Option<FeedInfo>,
    pub language: Option<String>, // some versions have this, some don't
    pub updated: i64,
}

#[derive(Debug, Deserialize, Clone)]
struct FeedInfo {
    pub title: String,
    pub url: String,
}

pub struct NativeFreshRSSClient {
    client: Client,
    config: FreshRSSConfig,
}

impl NativeFreshRSSClient {
    pub fn new(config: FreshRSSConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("Failed to create HTTP client"),
            config,
        }
    }

    /// Fetch unread articles for a specific user since the given timestamp.
    pub async fn fetch_unread_articles(
        &self,
        username: &str,
        password: Option<&str>,
        since: i64,
    ) -> anyhow::Result<Vec<Article>> {
        let url = format!("{}/api/entries.json", self.config.url);
        let password_to_use = password.unwrap_or(&self.config.password);

        let payload = serde_json::json!({
            "user": username,
            "password": password_to_use,
            "since": since,
            "limit": 500
        });

        let response = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("FreshRSS entries API error: {} - {}", status, body));
        }

        let entries: Vec<FreshRSSArticle> = response.json().await.map_err(|e| {
            anyhow::anyhow!("Failed to parse FreshRSS entries response as JSON: {}", e)
        })?;

        let mut article_vec = Vec::new();
        for entry in entries {
            let (feed_title, feed_url) = match &entry.feed {
                Some(feed) => (Some(feed.title.clone()), Some(feed.url.clone())),
                None => (None, None),
            };

            article_vec.push(Article {
                id: entry.id,
                title: entry.title,
                author: entry.author,
                content: entry.body,
                url: entry.link,
                feed_title,
                feed_url,
                language: entry.language,
                published: entry.updated,
            });
        }

        // Sort by date descending
        article_vec.sort_by_key(|a| Reverse(a.published));

        Ok(article_vec)
    }

    /// Mark articles as read using the native API.
    /// The native FreshRSS API marks articles via the `/api/entries.json` endpoint
    /// with `action=read` and article IDs.
    pub async fn mark_as_read(
        &self,
        username: &str,
        password: Option<&str>,
        article_ids: &[u64],
    ) -> anyhow::Result<()> {
        let url = format!("{}/api/entries.json", self.config.url);
        let password_to_use = password.unwrap_or(&self.config.password);

        // Mark all articles in a single request
        let payload = serde_json::json!({
            "user": username,
            "password": password_to_use,
            "ids": article_ids
        });

        let response = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Failed to mark articles as read: {} - {}",
                status,
                body
            ));
        }

        Ok(())
    }
}
