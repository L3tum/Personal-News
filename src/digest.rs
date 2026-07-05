use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::config::AppConfig;
use crate::email::EmailClient;
use crate::freshrss::FreshRSSClient;
use crate::llm::LlmClient;
use crate::qdrant::QdrantClientWrapper;

/// Retry an async operation with exponential backoff (1s, 2s, 4s).
/// Returns the first successful result, or the last error after max_retries attempts.
async fn retry_with_backoff<F, Fut, T>(max_retries: usize, operation: F) -> anyhow::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut delay = Duration::from_secs(1);
    let mut attempts = 0;
    loop {
        attempts += 1;
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempts > max_retries {
                    return Err(e);
                }
                log::warn!(
                    "Operation failed (attempt {}/{}), retrying in {:?}: {}",
                    attempts,
                    max_retries + 1,
                    delay,
                    e
                );
                sleep(delay).await;
                delay *= 2;
                if delay > Duration::from_secs(10) {
                    delay = Duration::from_secs(10);
                }
            }
        }
    }
}

/// Combine per-article semantic RAG with recent digest context
fn combine_rag_context(semantic: &[serde_json::Value], recent: &[serde_json::Value]) -> String {
    let mut parts = Vec::new();

    if !semantic.is_empty() {
        let text = semantic
            .iter()
            .filter_map(|v| v.get("summary").and_then(|s| s.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n");
        if !text.is_empty() {
            parts.push(format!("## Related Past Articles\n{}", text));
        }
    }

    if !recent.is_empty() {
        let text = recent
            .iter()
            .filter_map(|v| {
                let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("");
                let summary = v.get("summary").and_then(|s| s.as_str()).unwrap_or("");
                let digest_date = v.get("digest_date").and_then(|d| d.as_str()).unwrap_or("");
                if !summary.is_empty() {
                    Some(format!("- **{}** ({}): {}", title, digest_date, summary))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            parts.push(format!("## Recent Digest Context\n{}", text));
        }
    }

    parts.join("\n\n")
}

pub async fn generate_and_send_digest(
    config: Arc<AppConfig>,
    freshrss_client: Arc<FreshRSSClient>,
    qdrant_client: Arc<QdrantClientWrapper>,
    llm_client: Arc<LlmClient>,
    email_client: Arc<EmailClient>,
) -> anyhow::Result<()> {
    use crate::translate::LibreTranslateClient;
    let translate_client = LibreTranslateClient::new(
        config.libretranslate.url.clone(),
        config.libretranslate.api_key.clone(),
    );
    log::info!("Starting daily digest generation...");

    let now = Utc::now();
    let since = now.timestamp() - 86400; // last 24 hours
    let date_str = now.format("%Y-%m-%d").to_string();

    // For each user, fetch their unread articles with retry and per-user fault isolation
    for user_config in &config.users {
        log::info!("Processing user: {}", user_config.name);

        // Fetch unread articles for this user with retry-backoff
        let articles = match retry_with_backoff(3, || {
            freshrss_client.fetch_unread_articles(&user_config.freshrss_user, since)
        })
        .await
        {
            Ok(articles) => articles,
            Err(e) => {
                log::error!(
                    "Failed to fetch articles for {} after retries: {}. Skipping user.",
                    user_config.name,
                    e
                );
                continue;
            }
        };

        if articles.is_empty() {
            log::info!("No unread articles for {}", user_config.name);
            continue;
        }

        log::info!(
            "Found {} unread articles for {}",
            articles.len(),
            user_config.name
        );

        let mut article_summaries: HashMap<String, (String, Option<String>)> = HashMap::new();

        // Cache: feed_url -> detected language, to avoid redundant detection calls
        let mut feed_language_cache: HashMap<String, String> = HashMap::new();

        // Process each article: upsert, get RAG context, summarize, store summary immediately
        for article in &articles {
            // Upsert article with placeholder summary (no translation yet)
            qdrant_client
                .upsert_article(article, None, None, date_str.clone())
                .await
                .unwrap_or_else(|e| log::warn!("Failed to upsert {}: {}", article.title, e));

            // Get per-article RAG context (semantic + recent)
            let semantic_context = qdrant_client
                .retrieve_rag_context(&article.title, config.qdrant.top_k)
                .await
                .unwrap_or_default();
            let recent_context = qdrant_client
                .get_recent_articles(config.qdrant.context_window_days, 5)
                .await
                .unwrap_or_default();
            let combined_rag = combine_rag_context(&semantic_context, &recent_context);

            // Summarize this article
            let summary = llm_client
                .summarize_article(&combined_rag, &article.title, &article.content)
                .await
                .unwrap_or_else(|e| {
                    log::warn!("Failed to summarize {}: {}", article.title, e);
                    article.title.clone()
                });

            // Detect language and optionally translate for this user
            // Prefer FreshRSS's feed-level language, with per-feed detection cache as fallback
            let translated_summary = if let Some(ref target_lang) = user_config.target_language {
                // First, try FreshRSS's own language field (if the feed has one)
                // Fall back to per-feed detection cache, then to live detection
                let detected = match (
                    article.language.as_ref(),
                    feed_language_cache.get(&article.feed_url),
                ) {
                    (Some(feed_lang), _) => {
                        log::debug!("Using FreshRSS feed language: {}", feed_lang);
                        Some(feed_lang.clone())
                    }
                    (None, Some(cached)) => {
                        log::debug!("Using cached language: {}", cached);
                        Some(cached.clone())
                    }
                    (None, None) => {
                        // Detect language and cache it for this feed_url
                        match translate_client.detect_language(&summary).await {
                            Ok(lang) => {
                                log::info!(
                                    "Detected language {} for feed '{}' (cached for future articles)",
                                    lang,
                                    article.feed_title
                                );
                                feed_language_cache.insert(article.feed_url.clone(), lang.clone());
                                Some(lang)
                            }
                            Err(e) => {
                                log::warn!(
                                    "Language detection failed for {}: {} - using original summary",
                                    article.title,
                                    e
                                );
                                None
                            }
                        }
                    }
                };

                if let Some(ref detected) = detected {
                    // Only translate if the detected language differs from target
                    if *detected != *target_lang {
                        match translate_client
                            .translate(&summary, detected, target_lang)
                            .await
                        {
                            Ok(translated) => {
                                log::debug!(
                                    "Translated {} from {} to {} for {}",
                                    article.title,
                                    detected,
                                    target_lang,
                                    user_config.name
                                );
                                Some(translated)
                            }
                            Err(e) => {
                                log::warn!(
                                    "Translation failed for {}: {} - using original summary",
                                    article.title,
                                    e
                                );
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            // Store both summaries immediately
            let mut summary_map = HashMap::new();
            summary_map.insert(
                article.url.clone(),
                (summary.clone(), translated_summary.clone()),
            );
            if let Err(e) = qdrant_client
                .update_article_summaries(&date_str, &summary_map)
                .await
            {
                log::warn!("Failed to store summary for {}: {}", article.title, e);
            }

            // Store both original and translated summary for email
            article_summaries.insert(
                article.url.clone(),
                (summary.clone(), translated_summary.clone()),
            );
        }

        // Query "today's events" RAG for overall digest context, formatted as a string
        let today_rag_values = qdrant_client
            .retrieve_rag_context(&format!("{} events predictions reminders", date_str), 5)
            .await
            .unwrap_or_default();
        let today_rag = today_rag_values
            .iter()
            .filter_map(|v| v.get("summary").and_then(|s| s.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n");

        // Generate overall digest paragraph connecting stories
        let overall_digest = llm_client
            .generate_overall_digest(
                &today_rag,
                &article_summaries,
                &date_str,
                user_config.target_language.as_ref(),
            )
            .await
            .unwrap_or_else(|e| {
                log::warn!("Failed to generate overall digest: {}", e);
                "Unable to generate overall summary.".to_string()
            });

        log::info!("Generated digest for {}", user_config.name);

        // Mark articles as read in FreshRSS with retry
        let ids: Vec<u64> = articles.iter().map(|a| a.id).collect();
        if let Err(e) = retry_with_backoff(3, || {
            freshrss_client.mark_as_read(&user_config.freshrss_user, &ids)
        })
        .await
        {
            log::error!(
                "Failed to mark articles as read for {} after retries: {}. Continuing anyway.",
                user_config.name,
                e
            );
        }

        // Send email with retry-backoff
        let subject = format!("📰 Daily News Digest - {}", date_str);
        if let Err(e) = retry_with_backoff(3, || {
            email_client.send_digest(
                &subject,
                &overall_digest,
                std::slice::from_ref(&user_config.email),
            )
        })
        .await
        {
            log::error!(
                "Failed to send digest to {} after retries: {}. Skipping email.",
                user_config.name,
                e
            );
        }
    }

    log::info!("Digest generation complete!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_combine_rag_context_empty() {
        let semantic: Vec<serde_json::Value> = Vec::new();
        let recent: Vec<serde_json::Value> = Vec::new();
        let result = combine_rag_context(&semantic, &recent);
        assert!(result.is_empty());
    }

    #[test]
    fn test_combine_rag_context_with_data() {
        let semantic = vec![serde_json::json!({
            "summary": "Past event about X",
            "title": "Old article"
        })];
        let recent = vec![serde_json::json!({
            "summary": "Recent digest about Y",
            "title": "Recent article",
            "digest_date": "2025-07-01"
        })];
        let result = combine_rag_context(&semantic, &recent);
        assert!(result.contains("Past event about X"));
        assert!(result.contains("Recent digest about Y"));
        assert!(result.contains("## Recent Digest Context"));
    }

    #[test]
    fn test_parse_cron_time_valid() {
        use crate::parse_cron_time;
        assert_eq!(parse_cron_time("06:00").unwrap(), (6, 0));
        assert_eq!(parse_cron_time("14:30").unwrap(), (14, 30));
        assert_eq!(parse_cron_time("23:59").unwrap(), (23, 59));
    }

    #[test]
    fn test_parse_cron_time_invalid() {
        use crate::parse_cron_time;
        assert!(parse_cron_time("invalid").is_err());
        assert!(parse_cron_time("60:00").is_err());
        assert!(parse_cron_time("12:60").is_err());
    }
}
