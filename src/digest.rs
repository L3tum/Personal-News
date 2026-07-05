use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;

use crate::config::AppConfig;
use crate::email::EmailClient;
use crate::freshrss::FreshRSSClient;
use crate::ollama::OllamaClient;
use crate::qdrant::QdrantClientWrapper;

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
    ollama_client: Arc<OllamaClient>,
    email_client: Arc<EmailClient>,
) -> anyhow::Result<()> {
    log::info!("Starting daily digest generation...");

    let now = Utc::now();
    let since = now.timestamp() - 86400; // last 24 hours
    let date_str = now.format("%Y-%m-%d").to_string();

    // For each user, fetch their unread articles
    for user_config in &config.users {
        log::info!("Processing user: {}", user_config.name);

        // Fetch unread articles for this user
        let articles = freshrss_client
            .fetch_unread_articles(&user_config.freshrss_user, since)
            .await?;

        if articles.is_empty() {
            log::info!("No unread articles for {}", user_config.name);
            continue;
        }

        log::info!(
            "Found {} unread articles for {}",
            articles.len(),
            user_config.name
        );

        let mut article_summaries: HashMap<String, String> = HashMap::new();

        // Process each article: upsert, get RAG context, summarize, store summary immediately
        for article in &articles {
            // Upsert article with placeholder summary
            qdrant_client
                .upsert_article(article, None, date_str.clone())
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
            let summary = ollama_client
                .summarize_article(&combined_rag, &article.title, &article.content)
                .await
                .unwrap_or_else(|e| {
                    log::warn!("Failed to summarize {}: {}", article.title, e);
                    article.title.clone()
                });

            // Store summary immediately
            let mut summary_map = HashMap::new();
            summary_map.insert(article.url.clone(), summary.clone());
            if let Err(e) = qdrant_client.update_article_summaries(&summary_map).await {
                log::warn!("Failed to store summary for {}: {}", article.title, e);
            }
            article_summaries.insert(article.url.clone(), summary);
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
        let overall_digest = ollama_client
            .generate_overall_digest(&today_rag, &article_summaries, &date_str)
            .await
            .unwrap_or_else(|e| {
                log::warn!("Failed to generate overall digest: {}", e);
                "Unable to generate overall summary.".to_string()
            });

        log::info!("Generated digest for {}", user_config.name);

        // Mark articles as read in FreshRSS
        let ids: Vec<u64> = articles.iter().map(|a| a.id).collect();
        freshrss_client
            .mark_as_read(&user_config.freshrss_user, &ids)
            .await?;

        // Send email
        let subject = format!("📰 Daily News Digest - {}", date_str);
        email_client
            .send_digest(
                &subject,
                &overall_digest,
                std::slice::from_ref(&user_config.email),
            )
            .await?;
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
