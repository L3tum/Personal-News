use crate::config::OllamaConfig;
use reqwest::Client;

pub struct LlmClient {
    client: Client,
    config: OllamaConfig,
}

impl LlmClient {
    pub fn new(config: OllamaConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120)) // longer timeout for LLM
                .build()
                .expect("Failed to create HTTP client"),
            config,
        }
    }

    /// Generate a completion using OpenAI-compatible /v1/chat/completions API
    pub async fn chat(&self, prompt: &str) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {
                    "role": "system",
                    "content": "You are a news digest summarizer. You provide concise, accurate summaries with narrative continuity."
                },
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            "stream": false
        });

        let url = format!("{}/v1/chat/completions", self.config.url);
        let response = self.client.post(&url).json(&body).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await?;
            return Err(anyhow::anyhow!("LLM error: {} - {}", status, body_text));
        }

        let result: serde_json::Value = response.json().await?;
        let content = result["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Unexpected response format from LLM"))?
            .to_string();
        Ok(content)
    }

    /// Summarize a single article using RAG context
    pub async fn summarize_article(
        &self,
        rag_context: &str,
        title: &str,
        content: &str,
    ) -> anyhow::Result<String> {
        let content_preview = content.trim().chars().take(800).collect::<String>();
        let prompt = self
            .config
            .article_summary_prompt
            .replace("{rag_context}", rag_context)
            .replace("{article_title}", title)
            .replace("{article_content}", &content_preview);
        self.chat(&prompt).await
    }

    /// Generate an overall digest paragraph connecting related articles
    pub async fn generate_overall_digest(
        &self,
        today_rag: &str,
        article_summaries: &std::collections::HashMap<String, (String, Option<String>)>,
        date: &str,
        target_language: Option<&String>,
    ) -> anyhow::Result<String> {
        // Build article summaries list with links
        // For users with target_language, show only the target language summary (translated or original)
        let articles_text = article_summaries
            .iter()
            .map(|(url, (original, translated))| {
                let display_text = match target_language {
                    Some(_) => translated.as_deref().unwrap_or(original.as_str()),
                    None => original.as_str(),
                };
                format!("  - {display_text} ([Link]({url}))")
            })
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = self
            .config
            .summarizer_prompt
            .replace("{rag_context}", today_rag)
            .replace("{articles}", &articles_text)
            .replace("{date}", date);
        self.chat(&prompt).await
    }
}
