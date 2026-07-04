use chrono::Utc;
use std::sync::Arc;

use crate::config::AppConfig;
use crate::email::EmailClient;
use crate::freshrss::FreshRSSClient;
use crate::ollama::OllamaClient;
use crate::qdrant::QdrantClientWrapper;

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

        let articles_clone = articles.clone();

        // Parallel embedding and storage
        let tasks: Vec<_> = articles_clone
            .iter()
            .map(|article| {
                let qdrant = Arc::clone(&qdrant_client);
                let digest_date = date_str.clone();
                async move {
                    // Simple one-sentence summary
                    let summary = format!("Summary needed: {}", &article.title);
                    qdrant
                        .upsert_article(article, Some(summary), digest_date)
                        .await
                }
            })
            .collect();

        for task in tasks {
            if let Err(e) = task.await {
                log::warn!("Failed to upsert article: {}", e);
            }
        }

        // Build RAG query from actual article titles (semantic context)
        let rag_query = articles
            .iter()
            .take(5) // enough topics, not the whole batch
            .map(|a| a.title.clone())
            .collect::<Vec<_>>()
            .join(" ");
        let rag_context = qdrant_client
            .retrieve_rag_context(&rag_query, config.qdrant.top_k)
            .await?;

        // Also search for articles mentioning today's date (predictions, reminders)
        let today_rag = qdrant_client
            .retrieve_rag_context(&format!("{} event prediction", date_str), 5)
            .await?;

        let recent_context = qdrant_client
            .get_recent_articles(config.qdrant.context_window_days, 10)
            .await?;

        // Combine RAG context into a text block
        let rag_text = format_rag_context(&rag_context, &today_rag, &recent_context);

        // Generate digest using LLM
        let digest = ollama_client
            .generate_digest(&rag_text, &articles, &date_str)
            .await?;

        log::info!("Generated digest for {}", user_config.name);

        // Extract article summaries from the digest output
        let url_to_summary = extract_summaries_from_digest(&digest);
        log::info!("Extracted {} summaries from digest", url_to_summary.len());

        // Store summaries in Qdrant for future RAG context
        if !url_to_summary.is_empty() {
            qdrant_client
                .update_article_summaries(&url_to_summary)
                .await?;
        }

        // Mark articles as read in FreshRSS
        let ids: Vec<u64> = articles.iter().map(|a| a.id).collect();
        freshrss_client
            .mark_as_read(&user_config.freshrss_user, &ids)
            .await?;

        // Send email
        let subject = format!("📰 Daily News Digest - {}", date_str);
        email_client
            .send_digest(&subject, &digest, std::slice::from_ref(&user_config.email))
            .await?;
    }

    log::info!("Digest generation complete!");
    Ok(())
}

fn format_rag_context(
    rag_results: &[serde_json::Value],
    today_rag: &[serde_json::Value],
    recent: &[serde_json::Value],
) -> String {
    let mut sections = Vec::new();

    // Semantic context from topic query
    if !rag_results.is_empty() {
        let semantic = rag_results
            .iter()
            .filter_map(|v| v.get("summary").and_then(|s| s.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n");
        if !semantic.is_empty() {
            sections.push(format!("## Related Past Articles (Semantic)\n{}", semantic));
        }
    }

    // Today's date predictions/reminders
    if !today_rag.is_empty() {
        let today_items = today_rag
            .iter()
            .filter_map(|v| {
                let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("");
                let summary = v.get("summary").and_then(|s| s.as_str()).unwrap_or("");
                if !summary.is_empty() {
                    Some(format!("- **{}**: {}", title, summary))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !today_items.is_empty() {
            sections.push(format!(
                "## Today's Date Mentions (Predictions/Reminders)\n{}",
                today_items
            ));
        }
    }

    // Recent context
    if !recent.is_empty() {
        let recent_list = recent
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
        if !recent_list.is_empty() {
            sections.push(format!("## Recent Digest Context\n{}", recent_list));
        }
    }

    sections.join("\n\n")
}

/// Extract article summaries from the digest output.
/// Parses lines like: `- Title — summary — [Link](URL)`
fn extract_summaries_from_digest(digest: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();

    for line in digest.lines() {
        // Match lines starting with "- " (article summaries)
        if let Some(stripped) = line.strip_prefix("- ") {
            // Split by " — " and take the summary and link parts
            let parts: Vec<&str> = stripped.split(" — ").collect();
            if parts.len() >= 2 {
                let summary = parts[1].trim();
                // Extract URL from [Link](URL)
                if let Some(link_start) = summary.find("[Link](") {
                    if let Some(link_end) = summary.find(")") {
                        let url = summary[link_start..link_end + 1]
                            .trim()
                            .strip_prefix("[Link](")
                            .unwrap_or("");
                        if !url.is_empty() {
                            // Clean up summary (remove the link part)
                            let clean_summary = summary[..link_start].trim();
                            if !clean_summary.is_empty() {
                                map.insert(url.to_string(), clean_summary.to_string());
                            }
                        }
                    }
                } else {
                    // No link, maybe the summary is the whole thing without URL
                    // In that case, we can't match by URL, so skip
                    log::debug!("Could not extract URL from summary: {}", line);
                }
            }
        }
    }
    map
}
