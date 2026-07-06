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
    /// Uses the Google Reader compatibility API (greader.php).
    ///
    /// The API authenticates as the provided `username`, fetching articles for that user.
    /// If `password` is None, falls back to the global admin credentials.
    pub async fn fetch_unread_articles(
        &self,
        username: &str,
        password: Option<&str>,
        since: i64,
    ) -> anyhow::Result<Vec<Article>> {
        let url = format!("{}/api/greader.php", self.config.url);
        // stream=feed/ gives all unread entries across all feeds for the authenticated user
        let body = "cmd=reader/api/0/stream/contents&stream=feed/";

        let password_to_use = password.unwrap_or(&self.config.password);

        let response = self
            .client
            .post(&url)
            .basic_auth(username, Some(&password_to_use))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("FreshRSS API error: {} - {}", status, body));
        }

        let entries: Value = response.json().await?;
        let items = entries
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Unexpected FreshRSS response format"))?;

        let mut article_vec = Vec::new();
        for item in items {
            // FreshRSS greader response has items with:
            // id (string ID), title, canonical (array of {href}), summary (HTML content),
            // streamIds (feed IDs), ts (Unix timestamp), author (optional)
            let id_str = item
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing entry id"))?;

            // The id from greader is like "freshrss://feed-id/entry-id", extract the entry id
            let id = id_str
                .rsplit('/')
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| anyhow::anyhow!("Cannot parse entry id: {}", id_str))?;

            // Filter by since (last 24 hours) — greader API doesn't support since parameter
            let published = item.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
            if published < since {
                continue;
            }

            let title = item
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = item
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // canonical[0].href is the article URL
            let url = item
                .get("canonical")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|entry| entry.get("href"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Feed info from streamIds — get the first feed entry (usually just one)
            let (feed_title, feed_url) = item
                .get("streamIds")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|stream_id| {
                    // stream_id might be an object with title/url or a string like "feed/feed-id"
                    match stream_id {
                        Value::String(_s) => {
                            // "feed/feed-id" — we need to get the feed title from elsewhere
                            // Unfortunately we can't easily get the feed title from greader without another API call
                            // We'll leave it as "Unknown Feed" for now
                            Some(("Unknown Feed".to_string(), String::new()))
                        }
                        Value::Object(o) => {
                            let ft = o
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Unknown Feed")
                                .to_string();
                            let fu = o
                                .get("url")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            Some((ft, fu))
                        }
                        _ => None,
                    }
                })
                .unwrap_or_else(|| ("Unknown Feed".to_string(), String::new()));

            let author = item
                .get("author")
                .and_then(|v| v.as_str())
                .map(String::from);

            // greader doesn't have a language field, so we'll leave it as None
            // (language detection fallback in digest.rs will still work)
            let language = None;

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

    /// Mark articles as read using greader.php API.
    /// Note: greader API marks articles one by one, so we call it per article.
    ///
    /// Authenticates as the provided `username` (or falls back to global admin).
    pub async fn mark_as_read(
        &self,
        username: &str,
        password: Option<&str>,
        article_ids: &[u64],
    ) -> anyhow::Result<()> {
        let url = format!("{}/api/greader.php", self.config.url);
        let password_to_use = password.unwrap_or(&self.config.password);

        // Mark each article as read individually
        for article_id in article_ids {
            let body = format!("cmd=reader/api/0/edit/mark-as-read&i={}", article_id);

            let response = self
                .client
                .post(&url)
                .basic_auth(username, Some(&password_to_use))
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body)
                .send()
                .await?;

            if !response.status().is_success() {
                return Err(anyhow::anyhow!(
                    "Failed to mark article {} as read: {}",
                    article_id,
                    response.status()
                ));
            }
        }

        Ok(())
    }
}
