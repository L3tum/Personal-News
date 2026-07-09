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

    /// Fetch a single item by its ID using the Fever `item` endpoint.
    async fn fetch_item(
        &self,
        url: &str,
        api_key: &str,
        id: u64,
    ) -> anyhow::Result<Option<Article>> {
        let resp = self
            .client
            .post(format!("{}?api&item={}", url, id))
            .form(&[("api_key", api_key)])
            .send()
            .await?;
        let body = resp.text().await?;

        let value: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("Failed to parse Fever item {} response: {}", id, e);
                return Ok(None);
            }
        };

        let item = value
            .get("item")
            .ok_or_else(|| anyhow::anyhow!("Fever item {} response missing 'item' field", id))?;

        let fever_item: FeverItem = serde_json::from_value(item.clone())?;
        Ok(Some(Article {
            id: fever_item.id,
            title: fever_item.title,
            author: fever_item.author,
            content: fever_item.body,
            url: fever_item.url,
            feed_title: fever_item.feed_title,
            feed_url: fever_item.feed_url,
            published: fever_item.created,
            language: None,
        }))
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
        let auth_resp = self
            .client
            .post(format!("{}?api", url))
            .form(&[("api_key", &api_key)])
            .send()
            .await?;
        let auth_status = auth_resp.status();
        let auth_content_type: String = auth_resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|ct| ct.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let auth_body = auth_resp.text().await?;
        let auth_response: FeverAuthResponse = serde_json::from_str(&auth_body).map_err(|e| {
            log::error!(
                "Fever API auth returned non-JSON response:
Status: {}
Content-Type: {}
Body:
{}",
                auth_status,
                auth_content_type,
                auth_body
            );
            anyhow::anyhow!(
                "Failed to parse Fever auth response: {}\nServer returned: {}",
                e,
                auth_body
            )
        })?;

        if auth_response.auth != 1 {
            return Err(anyhow::anyhow!(
                "Fever API authentication failed for user {}. Check that you have an API password set (Settings > API password), or use the regular password if no API password is configured.",
                username
            ));
        }

        // Fetch unread items with full details
        let resp = self
            .client
            .post(format!("{}?api&unread_item_ids", url))
            .form(&[("api_key", &api_key)])
            .send()
            .await?;
        let status = resp.status();
        let content_type: String = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|ct| ct.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let items_body = resp.text().await?;

        let mut article_vec = Vec::new();
        let parse_result: Result<FeverItemsResponse, serde_json::Error> =
            serde_json::from_str(&items_body);

        match parse_result {
            Ok(items_response) => {
                for item in items_response.items {
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
                        language: None,
                    });
                }
            }
            Err(parse_err) => {
                // If the response was truncated (missing `items` field), fall back
                // to fetching items individually using the `item` endpoint
                let is_missing_items = parse_err.to_string().contains("missing field `items`");
                let truncated = !items_body.ends_with('}');
                if is_missing_items || truncated {
                    log::warn!(
                        "Fever API response truncated or missing items field (parse error: {}), \
                         falling back to individual item fetch",
                        parse_err
                    );

                    // Try to extract unread_item_ids from the partial JSON
                    // Using serde_json::Value with partial parsing
                    let partial_value: serde_json::Value = match serde_json::from_str(&items_body) {
                        Ok(v) => v,
                        Err(_) => {
                            log::error!(
                                "Cannot parse partial Fever API response as JSON:
Status: {}
Content-Type: {}
Body (last 200 chars):
{}",
                                status,
                                content_type,
                                &items_body
                                    .chars()
                                    .rev()
                                    .take(200)
                                    .collect::<String>()
                                    .chars()
                                    .rev()
                                    .collect::<String>()
                            );
                            return Err(anyhow::anyhow!(
                                "Fever API returned truncated/unparseable response: {}\nServer returned: {}",
                                parse_err,
                                items_body
                            ));
                        }
                    };

                    let unread_ids_str = partial_value.get("unread_item_ids")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Fever API response missing unread_item_ids field: {}\nServer returned: {}",
                                parse_err,
                                items_body
                            )
                        })?;

                    let ids: Vec<u64> = unread_ids_str
                        .split(',')
                        .filter(|s| !s.trim().is_empty())
                        .filter_map(|s| s.trim().parse::<u64>().ok())
                        .collect();

                    log::debug!("Fetched {} item IDs, fetching individually", ids.len());

                    // Fetch items individually, batching to avoid too many requests
                    for id in ids {
                        let item = self.fetch_item(&url, &api_key, id).await?;
                        if let Some(article) = item {
                            if article.published < since {
                                continue;
                            }
                            article_vec.push(article);
                        }
                    }
                } else {
                    // Some other JSON error — report it
                    log::error!(
                        "Fever API unread_item_ids returned non-JSON response:
Status: {}
Content-Type: {}
Body:
{}",
                        status,
                        content_type,
                        items_body
                    );
                    return Err(anyhow::anyhow!(
                        "Failed to parse Fever items response: {}\nServer returned: {}",
                        parse_err,
                        items_body
                    ));
                }
            }
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
