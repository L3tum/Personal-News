use anyhow;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::time::Duration;
use std::sync::Arc;
use tokio::time;

use crate::config::AppConfig;
use crate::freshrss::{Article, FreshRSSClient};
use crate::qdrant::QdrantClientWrapper;
use crate::ollama::OllamaClient;
use crate::email::EmailClient;

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

        log::info!("Found {} unread articles for {}", articles.len(), user_config.name);

        // Store each article in Qdrant with a temporary summary (empty for now)
        let articles_with_summaries = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let articles_clone = articles.clone();

        // Parallel embedding and storage
        let tasks: Vec<_> = articles_clone.iter()
            .map(|article| {
                let qdrant = Arc::clone(&qdrant_client);
                let digest_date = date_str.clone();
                async move {
                    // Simple one-sentence summary
                    let summary = format!(
                        "Summary needed: {}",
                        &article.title
                    );
                    qdrant.upsert_article(article, Some(summary), digest_date)
                        .await
                }
            })
            .collect();

        for task in tasks {
            if let Err(e) = task.await {
                log::warn!("Failed to upsert article: {}", e);
            }
        }

        // Retrieve RAG context
        let rag_context = qdrant_client
            .retrieve_rag_context(&format!("latest news digest context"), config.qdrant.top_k)
            .await?;

        let recent_context = qdrant_client
            .get_recent_articles(config.qdrant.context_window_days, 10)
            .await?;

        // Combine RAG context into a text block
        let rag_text = format_rag_context(&rag_context, &recent_context);

        // Generate digest using LLM
        let digest = ollama_client
            .generate_digest(&rag_text, &articles, &date_str)
            .await?;

        log::info!("Generated digest for {}", user_config.name);

        // Store each article's summary (extract from digest)
        // For now, we just mark articles as read in FreshRSS
        let ids: Vec<u64> = articles.iter().map(|a| a.id).collect();
        freshrss_client
            .mark_as_read(&user_config.freshrss_user, &ids)
            .await?;

        // Send email
        let subject = format!("📰 Daily News Digest - {}", date_str);
        email_client
            .send_digest(&subject, &digest, &[user_config.email.clone()])
            .await?;
    }

    log::info!("Digest generation complete!");
    Ok(())
}

fn format_rag_context(rag_results: &[serde_json::Value], recent: &[serde_json::Value]) -> String {
    let mut sections = Vec::new();

    // Semantic context
    if !rag_results.is_empty() {
        let semantic = rag_results.iter()
            .filter_map(|v| v.get("summary").and_then(|s| s.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n");
        if !semantic.is_empty() {
            sections.push(format!("## Related Past Articles (Semantic)\n{}", semantic));
        }
    }

    // Recent context
    if !recent.is_empty() {
        let recent_list = recent.iter()
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
        if !recent_list.is_empty() {
            sections.push(format!("## Recent Digest Context\n{}", recent_list));
        }
    }

    sections.join("\n\n")
}
