use anyhow;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use crate::config::OllamaConfig;

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
        let response = self.client.post(&url)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let body = response.text().await?;
            return Err(anyhow::anyhow!("Ollama error: {} - {}", response.status(), body));
        }

        let result: ChatResponse = response.json().await?;
        Ok(result.message.content)
    }

    /// Generate a full digest from articles and context
    pub async fn generate_digest(
        &self,
        rag_context: &str,
        articles: &[crate::freshrss::Article],
        date: &str,
    ) -> anyhow::Result<String> {
        // Build articles list as markdown
        let articles_text = articles.iter().enumerate()
            .map(|(i, article)| {
                let content_preview = article.content.trim().chars().take(500).collect::<String>();
                format!(
                    "### {} - {} ({})
**Content preview**: {}
**URL**: {}",
                    i + 1,
                    article.title,
                    article.feed_title,
                    content_preview,
                    article.url,
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = self.config.summarizer_prompt
            .replace("{rag_context}", rag_context)
            .replace("{articles}", &articles_text)
            .replace("{date}", date);

        self.chat(&prompt).await
    }
}
