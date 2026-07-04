use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub freshrss: FreshRSSConfig,
    pub qdrant: QdrantConfig,
    pub ollama: OllamaConfig,
    pub smtp: SmtpConfig,
    pub users: Vec<UserConfig>,
    pub cron: CronConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FreshRSSConfig {
    pub url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct QdrantConfig {
    pub url: String,
    pub api_key: Option<String>,
    pub collection: String,
    pub embedding_model: String, // e.g., "nomic-embed-text" or "all-MiniLM-L6-v2"
    pub context_window_days: i64,
    pub top_k: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OllamaConfig {
    pub url: String,
    pub model: String, // e.g., "llama3.1" or "mistral"
    pub summarizer_prompt: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from: String,
    pub tls_mode: String, // "none" | "starttls" | "tls" (default "starttls")
}

#[derive(Debug, Deserialize, Clone)]
pub struct UserConfig {
    pub name: String,
    pub freshrss_user: String,
    pub email: String,
    #[allow(dead_code)]
    pub shared_feeds: Option<Vec<String>>, // feed IDs to share with other users
}

#[derive(Debug, Deserialize, Clone)]
pub struct CronConfig {
    pub time: String, // "06:00" for 6 AM
    pub timezone: String,
}

impl AppConfig {
    pub fn load_from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok(); // load .env if exists

        Ok(AppConfig {
            freshrss: FreshRSSConfig {
                url: std::env::var("FRESHRSS_URL")?,
                username: std::env::var("FRESHRSS_USERNAME")?,
                password: std::env::var("FRESHRSS_PASSWORD")?,
            },
            qdrant: QdrantConfig {
                url: std::env::var("QDRANT_URL")?,
                api_key: std::env::var("QDRANT_API_KEY").ok(),
                collection: std::env::var("QDRANT_COLLECTION").unwrap_or("articles".to_string()),
                embedding_model: std::env::var("EMBEDDING_MODEL")
                    .unwrap_or("nomic-embed-text".to_string()),
                context_window_days: std::env::var("CONTEXT_WINDOW_DAYS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(30),
                top_k: std::env::var("RAG_TOP_K")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(5),
            },
            ollama: OllamaConfig {
                url: std::env::var("OLLAMA_URL").unwrap_or("http://localhost:11434".to_string()),
                model: std::env::var("LLM_MODEL").unwrap_or("llama3.1".to_string()),
                summarizer_prompt: std::env::var("LLM_PROMPT")
                    .ok()
                    .unwrap_or(Self::default_prompt()),
            },
            smtp: SmtpConfig {
                host: std::env::var("SMTP_HOST")?,
                port: std::env::var("SMTP_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(587),
                username: std::env::var("SMTP_USERNAME")?,
                password: std::env::var("SMTP_PASSWORD")?,
                from: std::env::var("SMTP_FROM")?,
                tls_mode: std::env::var("SMTP_TLS_MODE").unwrap_or("starttls".to_string()),
            },
            users: std::env::var("USERS")
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(vec![UserConfig {
                    name: "Default".to_string(),
                    freshrss_user: "admin".to_string(),
                    email: std::env::var("DEFAULT_EMAIL")?,
                    shared_feeds: None,
                }]),
            cron: CronConfig {
                time: std::env::var("CRON_TIME").unwrap_or("06:00".to_string()),
                timezone: std::env::var("CRON_TIMEZONE").unwrap_or("UTC".to_string()),
            },
        })
    }

    fn default_prompt() -> String {
        String::from(
            r#"You are an expert RSS digest summarizer. 

## Context from Recent Articles
{rag_context}

## New Articles
{articles}

## Your Task
1. For each new article, provide a very short summary (one sentence, max 30 words).
2. Include a clickable link to the article in the format: [Link](URL).
3. Create an overall digest paragraph (max 200 words) that connects related stories, references past events, and provides narrative continuity.
4. If the context mentions past events, use that to provide continuity (e.g., "The war that started last week has now..." ).

Format your response as:

**Daily Digest: {date}**

**Article Summaries:**
- {article title} — {summary} — [Link]({url})
- {article title} — {summary} — [Link]({url})

**Overall Summary:**
{overall_digest_paragraph}

Output plain Markdown, no extra formatting."#,
        )
    }
}
