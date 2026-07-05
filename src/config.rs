use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub freshrss: FreshRSSConfig,
    pub qdrant: QdrantConfig,
    pub ollama: OllamaConfig,
    pub libretranslate: LibreTranslateConfig,
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
    pub embedding_url: String,   // OpenAI-compatible embedding endpoint base URL
    pub embedding_dim: usize,    // vector dimension for the collection (default 768)
    pub context_window_days: i64,
    pub top_k: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LibreTranslateConfig {
    pub url: String, // e.g., "http://localhost:5000"
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OllamaConfig {
    pub url: String,
    pub model: String, // e.g., "llama3.1" or "mistral"
    pub summarizer_prompt: String,
    pub article_summary_prompt: String,
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
    /// Optional target language for translated summaries (ISO 639-1, e.g., "en", "de", "fr").
    /// If set, summaries will be translated to this language. If None, no translation.
    pub target_language: Option<String>,
    // TODO: shared_feeds — allow users to share feeds (reserved for future use)
    #[allow(dead_code)]
    pub shared_feeds: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CronConfig {
    pub time: String, // "06:00" for 6 AM
    pub timezone: String,
}

impl AppConfig {
    pub fn load_from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok(); // load .env if exists

        // Helper to get a required environment variable with a clear error message
        macro_rules! required_env {
            ($name:expr) => {
                std::env::var($name)
                    .map_err(|_| anyhow::anyhow!("Environment variable {} is not set", $name))
            };
        }

        Ok(AppConfig {
            freshrss: FreshRSSConfig {
                url: required_env!("FRESHRSS_URL")?,
                username: required_env!("FRESHRSS_USERNAME")?,
                password: required_env!("FRESHRSS_PASSWORD")?,
            },
            qdrant: QdrantConfig {
                url: required_env!("QDRANT_URL")?,
                api_key: std::env::var("QDRANT_API_KEY").ok(),
                collection: std::env::var("QDRANT_COLLECTION").unwrap_or("articles".to_string()),
                embedding_model: std::env::var("EMBEDDING_MODEL")
                    .unwrap_or("nomic-embed-text".to_string()),
                embedding_url: std::env::var("EMBEDDING_URL").unwrap_or_else(|_| {
                    std::env::var("OLLAMA_URL").unwrap_or("http://localhost:11434".to_string())
                }),
                embedding_dim: std::env::var("EMBEDDING_DIM")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(768),
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
                article_summary_prompt: std::env::var("ARTICLE_SUMMARY_PROMPT")
                    .ok()
                    .unwrap_or(Self::default_article_prompt()),
            },
            libretranslate: LibreTranslateConfig {
                url: required_env!("LIBRETRANSLATE_URL")?,
                api_key: std::env::var("LIBRETRANSLATE_API_KEY").ok(),
            },
            smtp: SmtpConfig {
                host: required_env!("SMTP_HOST")?,
                port: std::env::var("SMTP_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(587),
                username: required_env!("SMTP_USERNAME")?,
                password: required_env!("SMTP_PASSWORD")?,
                from: required_env!("SMTP_FROM")?,
                tls_mode: std::env::var("SMTP_TLS_MODE").unwrap_or("starttls".to_string()),
            },
            users: std::env::var("USERS")
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(vec![UserConfig {
                    name: "Default".to_string(),
                    freshrss_user: "admin".to_string(),
                    email: std::env::var("DEFAULT_EMAIL")
                        .map_err(|_| {
                            anyhow::anyhow!(
                                "Environment variable DEFAULT_EMAIL is not set (required when USERS is not provided)"
                            )
                        })?,
                    target_language: None,
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

## Context from Today's Events and Predictions
{rag_context}

## Article Summaries (for reference)
{articles}

## Your Task
Create an overall digest paragraph (max 200 words) that connects the articles together narratively, referencing the context from today's events and predictions. Include the link to each article after its summary if needed.

Format:
**Daily Digest: {date}**

**Article Summaries:**
{articles}

**Overall Summary:**
{overall_digest_paragraph}

Output plain Markdown, no extra formatting."#,
        )
    }

    fn default_article_prompt() -> String {
        String::from(
            r#"## Context: Related Past Articles
{rag_context}

## New Article
{article_title}
{article_content}

## Your Task
Write a one-sentence summary (max 30 words) of the article, referencing any relevant past context. Just return the summary text, nothing else."#,
        )
    }
}
