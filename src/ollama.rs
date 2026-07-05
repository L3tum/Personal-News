use crate::config::OllamaConfig;
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub message: ChatMessage,
}

pub struct OllamaClient {
    client: Client,
    config: OllamaConfig,
}

impl OllamaClient {
    pub fn new(config: OllamaConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120)) // longer timeout for LLM
                .build()
                .expect("Failed to create HTTP client"),
            config,
        }
    }

    /// Generate a completion with chat completion API
    pub async fn chat(&self, prompt: &str) -> anyhow::Result<String> {
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![ChatMessage {
                role: "system".to_string(),
                content: String::from(
                    "You are a news digest summarizer. You provide concise, accurate summaries with narrative continuity.",
                ),
            }, ChatMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
            stream: false,
        };

        let url = format!("{}/api/chat", self.config.url);
        let response = self.client.post(&url).json(&request).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            return Err(anyhow::anyhow!("Ollama error: {} - {}", status, body));
        }

        let result: ChatResponse = response.json().await?;
        Ok(result.message.content)
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
        article_summaries: &std::collections::HashMap<String, String>,
        date: &str,
    ) -> anyhow::Result<String> {
        // Build article summaries list with links
        let articles_text = article_summaries
            .iter()
            .map(|(url, summary)| format!("  - {summary} ([Link]({url}))"))
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
