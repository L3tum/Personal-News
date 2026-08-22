//! FreshRSS [Fever API](https://freshrss.github.io/FreshRSS/en/developers/06_Fever_API.html) client.
//!
//! Verified against the FreshRSS 1.27-1.29.x implementation (`p/api/fever.php`),
//! which is what `freshrss/freshrss:latest` currently ships.
//!
//! ## Authentication
//!
//! The Fever API authenticates with a **per-user API password**, which is
//! separate from the web login password. Each user must set it in the FreshRSS
//! web UI (Settings -> Profile -> "External access via API" -> "API password"),
//! and an administrator must have enabled "Allow API access" in the system
//! settings (Authentication section). A user without an API password can never
//! authenticate.
//!
//! `api_key = MD5("username:api_password")` is sent as a POST form field.
//!
//! ## Server-side quirks handled here (see `p/api/fever.php`)
//!
//! - `items` (also with `with_ids`) returns **at most 50 entries per request**
//!   (hard `LIMIT 50`), so we fetch in chunks of 50.
//! - `items` returns read *and* unread entries; we filter on `is_read`.
//! - Items only carry `feed_id`; feed titles/URLs come from the `feeds` action.
//! - There is **no** single-item endpoint; fetch specific entries with
//!   `items&with_ids=<csv>` (max 50 ids per request).
//! - `unread_item_ids` returns a comma-separated string (empty when none).
//! - Marking items is a *write* operation: `mark=item&as=read` with a single
//!   `id` or a comma-separated `with_ids`, sent as POST form fields.
//! - HTTP 503 "Service Unavailable" means API access is disabled system-wide.

use crate::config::FreshRSSConfig;
use md5::compute;
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::time::Duration;

/// The Fever API returns at most 50 entries per `items` request (server-side LIMIT).
pub const ITEMS_PER_PAGE: usize = 50;

/// Safety cap: at most this many unread articles are summarized per user per
/// digest run. Anything beyond that stays unread and is picked up on later runs.
pub const MAX_ARTICLES_PER_RUN: usize = 1000;

/// An article, as consumed by the digest pipeline.
#[derive(Debug, Clone)]
pub struct Article {
    pub id: u64,
    pub title: String,
    pub author: Option<String>,
    /// HTML content of the article (Fever `html` field).
    pub content: String,
    pub url: String,
    pub feed_title: String,
    pub feed_url: Option<String>,
    /// ISO 639-1 language code. The Fever API does not provide one, so this is
    /// always `None`; digest falls back to LibreTranslate language detection.
    pub language: Option<String>,
    /// Publication timestamp in Unix seconds (Fever `created_on_time`).
    pub published: i64,
}

/// Raw item as returned by the Fever API `items` action.
#[derive(Debug, Deserialize)]
struct FeverItem {
    id: u64,
    #[serde(default)]
    feed_id: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    html: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    is_read: Option<u64>,
    #[serde(default)]
    created_on_time: Option<i64>,
}

/// Feed object as returned by the Fever API `feeds` action.
#[derive(Debug, Deserialize)]
struct FeverFeed {
    id: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthResponse {
    #[serde(default)]
    auth: u64,
}

#[derive(Debug, Deserialize)]
struct FeedsResponse {
    #[serde(default)]
    feeds: Vec<FeverFeed>,
}

#[derive(Debug, Deserialize)]
struct UnreadIdsResponse {
    #[serde(default)]
    unread_item_ids: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ItemsResponse {
    #[serde(default)]
    items: Vec<FeverItem>,
}

#[derive(Debug, Deserialize)]
struct MarkResponse {
    #[serde(default)]
    auth: u64,
}

/// Compute the Fever API key: MD5("username:api_password").
///
/// Note this is the *API password*, not the login password.
pub fn api_key(username: &str, api_password: &str) -> String {
    let digest = compute(format!("{username}:{api_password}").as_bytes());
    format!("{digest:x}")
}

/// Parse the comma-separated id list returned by `unread_item_ids`.
fn parse_id_list(list: &str) -> Vec<u64> {
    list.split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

/// Convert a raw Fever item into an [`Article`], applying the read and
/// `since` filters. Returns `None` if the item should be skipped.
///
/// Note: FreshRSS returns titles/authors with HTML-special characters replaced
/// by full-width Unicode lookalikes (e.g. `&` → `＆`); that is server-side
/// behavior, not a bug.
fn item_to_article(
    item: &FeverItem,
    feed_map: &HashMap<u64, (String, String)>,
    since: i64,
) -> Option<Article> {
    // Defensive: unread_item_ids and items can drift between requests.
    if item.is_read.unwrap_or(0) == 1 {
        return None;
    }
    let published = item.created_on_time.unwrap_or(0);
    if published < since {
        return None;
    }

    let feed_id = item.feed_id.unwrap_or(0);
    let (feed_title, feed_url) = match feed_map.get(&feed_id) {
        Some((title, url)) => (title.clone(), Some(url.clone())),
        None => (format!("Feed #{feed_id}"), None),
    };

    Some(Article {
        id: item.id,
        title: item.title.clone().unwrap_or_default(),
        author: item
            .author
            .as_ref()
            .filter(|a| !a.trim().is_empty())
            .cloned(),
        content: item.html.clone().unwrap_or_default(),
        url: item.url.clone().unwrap_or_default(),
        feed_title,
        feed_url,
        language: None,
        published,
    })
}

fn truncate(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        format!("{}...", trimmed.chars().take(max_chars).collect::<String>())
    }
}

pub struct FeverClient {
    client: Client,
    config: FreshRSSConfig,
}

impl FeverClient {
    pub fn new(config: FreshRSSConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("Failed to create HTTP client"),
            config,
        }
    }

    /// `api_password` falls back to the global one from the config.
    fn resolve_api_password(&self, api_password: Option<&str>) -> String {
        api_password
            .map(str::to_owned)
            .unwrap_or_else(|| self.config.api_password.clone())
    }

    /// POST to the Fever endpoint.
    ///
    /// `query` is the *read* action and must go in the query string
    /// (e.g. `"api&feeds"`); `form` carries extra POST form fields
    /// (*write* actions such as `mark`), in addition to `api_key`.
    async fn request<T: DeserializeOwned>(
        &self,
        api_key: &str,
        query: &str,
        form: &[(&str, &str)],
    ) -> anyhow::Result<T> {
        let url = format!(
            "{}/api/fever.php?{}",
            self.config.url.trim_end_matches('/'),
            query
        );
        let mut body: Vec<(&str, &str)> = vec![("api_key", api_key)];
        body.extend_from_slice(form);

        let response = self.client.post(&url).form(&body).send().await?;
        let status = response.status();
        let text = response.text().await?;

        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Err(anyhow::anyhow!(
                "FreshRSS returned 503 Service Unavailable: API access is disabled \
                 system-wide. An administrator must enable it in the FreshRSS web UI \
                 (Settings > Authentication > 'Allow API access (required for mobile apps)')."
            ));
        }
        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "FreshRSS Fever API error: {} — {}",
                status,
                truncate(&text, 300)
            ));
        }

        serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse FreshRSS Fever API response for '{}': {} — body: {}",
                query,
                e,
                truncate(&text, 300)
            )
        })
    }

    /// Fetch unread articles of a single user, published at or after `since`
    /// (Unix seconds), newest first.
    ///
    /// `api_password` is the user's FreshRSS **API password** (not the login
    /// password); when `None`, the global `FRESHRSS_API_PASSWORD` is used.
    pub async fn fetch_unread_articles(
        &self,
        username: &str,
        api_password: Option<&str>,
        since: i64,
    ) -> anyhow::Result<Vec<Article>> {
        let key = api_key(username, &self.resolve_api_password(api_password));

        // 1. Verify authentication.
        let auth: AuthResponse = self.request(&key, "api", &[]).await?;
        if auth.auth != 1 {
            return Err(anyhow::anyhow!(
                "FreshRSS Fever API authentication failed for user '{username}'. \
                 The Fever API uses the per-user *API password* (FreshRSS web UI: \
                 Settings > Profile > 'External access via API' > 'API password'), \
                 NOT the login password. Every user must have an API password set, \
                 and it must match the configured freshrss_api_password / \
                 FRESHRSS_API_PASSWORD."
            ));
        }

        // 2. Map feed id -> (title, url) for feed metadata.
        let feeds: FeedsResponse = self.request(&key, "api&feeds", &[]).await?;
        let feed_map: HashMap<u64, (String, String)> = feeds
            .feeds
            .into_iter()
            .map(|f| {
                (
                    f.id,
                    (f.title.unwrap_or_default(), f.url.unwrap_or_default()),
                )
            })
            .collect();

        // 3. Collect unread entry ids (comma-separated string, empty when none).
        let unread: UnreadIdsResponse = self.request(&key, "api&unread_item_ids", &[]).await?;
        let unread_ids = parse_id_list(&unread.unread_item_ids.unwrap_or_default());
        if unread_ids.is_empty() {
            log::debug!("No unread articles for user {username}");
            return Ok(Vec::new());
        }
        log::info!(
            "User {username}: {} unread article(s) in FreshRSS",
            unread_ids.len()
        );

        // `unread_item_ids` is returned oldest-first and has no server-side cap.
        // Keep only the newest ids we can possibly use, so a large backlog
        // cannot blow up the request count. The `since` filter below drops
        // articles older than the digest window anyway.
        let max_ids = MAX_ARTICLES_PER_RUN.div_ceil(ITEMS_PER_PAGE) * ITEMS_PER_PAGE;
        let unread_ids: Vec<u64> = if unread_ids.len() > max_ids {
            log::info!("User {username}: capping fetch to the {max_ids} newest unread articles");
            unread_ids[unread_ids.len() - max_ids..].to_vec()
        } else {
            unread_ids
        };

        // 4. Fetch items in chunks of 50 (server-side page limit).
        let mut articles = Vec::new();
        for chunk in unread_ids.chunks(ITEMS_PER_PAGE) {
            let with_ids: String = chunk
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let items: ItemsResponse = self
                .request(&key, &format!("api&items&with_ids={with_ids}"), &[])
                .await?;
            for item in &items.items {
                if let Some(article) = item_to_article(item, &feed_map, since) {
                    articles.push(article);
                }
            }
        }

        // Cap to the most recent N articles so a large backlog cannot blow up a run.
        if articles.len() > MAX_ARTICLES_PER_RUN {
            log::warn!(
                "User {username}: {} unread articles within the last day, only summarizing \
                 the {MAX_ARTICLES_PER_RUN} most recent; the rest stay unread for later runs",
                articles.len()
            );
            articles.sort_by_key(|a| Reverse(a.published));
            articles.truncate(MAX_ARTICLES_PER_RUN);
        }

        articles.sort_by_key(|a| Reverse(a.published));
        Ok(articles)
    }

    /// Mark articles as read in batches (Fever API write action).
    pub async fn mark_as_read(
        &self,
        username: &str,
        api_password: Option<&str>,
        article_ids: &[u64],
    ) -> anyhow::Result<()> {
        if article_ids.is_empty() {
            return Ok(());
        }
        let key = api_key(username, &self.resolve_api_password(api_password));

        for chunk in article_ids.chunks(ITEMS_PER_PAGE) {
            let with_ids: String = chunk
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let mark: MarkResponse = self
                .request(
                    &key,
                    "api",
                    &[
                        ("mark", "item"),
                        ("as", "read"),
                        ("with_ids", with_ids.as_str()),
                    ],
                )
                .await?;
            if mark.auth != 1 {
                return Err(anyhow::anyhow!(
                    "FreshRSS Fever API authentication failed while marking articles as read \
                     for user '{username}'"
                ));
            }
        }

        log::info!(
            "Marked {} article(s) as read for user {username}",
            article_ids.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_matches_md5_of_user_and_password() {
        // Verified independently: echo -n "ab:c" | md5sum
        assert_eq!(api_key("ab", "c"), "9a98e4e96f6f66fa77ad64f3999ebfd7");
        // And it must be the hash of "user:password" (colon-joined), not of the
        // username or password alone.
        assert_ne!(api_key("ab", "c"), md5_hex("ab"));
        assert_ne!(api_key("ab", "c"), md5_hex("c"));
    }

    fn md5_hex(s: &str) -> String {
        format!("{:x}", compute(s.as_bytes()))
    }

    #[test]
    fn api_key_is_lowercase_hex() {
        let key = api_key("kevin", "freshrss");
        assert_eq!(key.len(), 32);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(key, key.to_lowercase());
    }

    #[test]
    fn parse_id_list_handles_comma_separated_ids() {
        assert_eq!(parse_id_list("1,2,3"), vec![1, 2, 3]);
        assert_eq!(parse_id_list(""), Vec::<u64>::new());
        assert_eq!(parse_id_list(" 7 , , 9 "), vec![7, 9]);
        assert_eq!(parse_id_list("42"), vec![42]);
    }

    #[test]
    fn item_mapping_uses_feed_map_and_filters() {
        let mut feed_map = HashMap::new();
        feed_map.insert(
            3,
            ("My Feed".to_string(), "https://example.com/rss".to_string()),
        );

        let item = FeverItem {
            id: 101,
            feed_id: Some(3),
            title: Some("Hello".to_string()),
            author: Some("  ".to_string()),
            html: Some("<p>Body</p>".to_string()),
            url: Some("https://example.com/article".to_string()),
            is_read: Some(0),
            created_on_time: Some(1_700_000_100),
        };

        let article = item_to_article(&item, &feed_map, 1_700_000_000).unwrap();
        assert_eq!(article.id, 101);
        assert_eq!(article.title, "Hello");
        assert_eq!(article.content, "<p>Body</p>");
        assert_eq!(article.url, "https://example.com/article");
        assert_eq!(article.feed_title, "My Feed");
        assert_eq!(article.feed_url.as_deref(), Some("https://example.com/rss"));
        assert!(article.author.is_none(), "whitespace author becomes None");
        assert!(article.language.is_none());
        assert_eq!(article.published, 1_700_000_100);
    }

    #[test]
    fn item_mapping_skips_read_and_old_items() {
        let feed_map = HashMap::new();

        let read_item = FeverItem {
            id: 1,
            feed_id: None,
            title: None,
            author: None,
            html: None,
            url: None,
            is_read: Some(1),
            created_on_time: Some(2_000_000_000),
        };
        assert!(item_to_article(&read_item, &feed_map, 0).is_none());

        let old_item = FeverItem {
            id: 2,
            feed_id: None,
            title: None,
            author: None,
            html: None,
            url: None,
            is_read: Some(0),
            created_on_time: Some(999),
        };
        assert!(item_to_article(&old_item, &feed_map, 1_000).is_none());
    }

    #[test]
    fn item_mapping_unknown_feed_gets_placeholder_title() {
        let feed_map = HashMap::new();
        let item = FeverItem {
            id: 5,
            feed_id: Some(77),
            title: None,
            author: None,
            html: None,
            url: None,
            is_read: Some(0),
            created_on_time: Some(2_000_000_000),
        };
        let article = item_to_article(&item, &feed_map, 0).unwrap();
        assert_eq!(article.feed_title, "Feed #77");
        assert_eq!(article.feed_url, None);
    }

    #[test]
    fn items_per_page_is_fever_limit() {
        // Guard against accidentally changing the chunk size: FreshRSS caps
        // `items` requests at 50 entries (LIMIT 50), larger chunks silently drop data.
        assert_eq!(ITEMS_PER_PAGE, 50);
    }
}
