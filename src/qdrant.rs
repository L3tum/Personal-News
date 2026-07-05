use qdrant_client::qdrant::{
    Condition, CreateCollectionBuilder, Distance, Filter, PointStruct, QueryPointsBuilder,
    ScrollPointsBuilder, SetPayloadPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use serde::{Deserialize, Serialize};

use crate::config::QdrantConfig;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArticlePayload {
    pub title: String,
    pub url: String,
    pub feed_title: String,
    pub feed_url: String,
    pub author: Option<String>,
    pub content: String,
    pub summary: Option<String>,
    pub published: i64,
    pub digest_date: String,
}

pub struct QdrantClientWrapper {
    qdrant: Qdrant,
    config: QdrantConfig,
    http_client: reqwest::Client,
}

impl QdrantClientWrapper {
    pub async fn new(config: QdrantConfig) -> anyhow::Result<Self> {
        let mut builder = Qdrant::from_url(&config.url);
        if let Some(api_key) = &config.api_key {
            builder = builder.api_key(api_key.as_str());
        }
        let qdrant = builder
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to connect to Qdrant: {e}"))?;

        // Create a shared HTTP client for embedding requests
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to create HTTP client for embeddings: {e}"))?;

        let wrapper = Self {
            qdrant,
            config,
            http_client,
        };
        wrapper.ensure_collection().await?;
        Ok(wrapper)
    }

    async fn ensure_collection(&self) -> anyhow::Result<()> {
        let collections = self
            .qdrant
            .list_collections()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list collections: {e}"))?;
        let exists = collections
            .collections
            .iter()
            .any(|c| c.name == self.config.collection);
        if !exists {
            self.qdrant
                .create_collection(
                    CreateCollectionBuilder::new(&self.config.collection).vectors_config(
                        VectorParamsBuilder::new(
                            self.config.embedding_dim as u64,
                            Distance::Cosine,
                        ),
                    ),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create collection: {e}"))?;
            log::info!(
                "Created collection: {} with {} dimensions",
                self.config.collection,
                self.config.embedding_dim
            );
        }
        Ok(())
    }

    pub async fn embed_text(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let url = format!("{}/v1/embeddings", self.config.embedding_url);
        let response = self
            .http_client
            .post(&url)
            .json(&serde_json::json!({
                "model": self.config.embedding_model,
                "input": text,
            }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch embedding from {}: {}", url, e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Embedding API error: {} - {}",
                status,
                body
            ));
        }

        let body: serde_json::Value = response.json().await?;
        let embedding = body["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Unexpected embedding format"))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();
        Ok(embedding)
    }

    pub async fn upsert_article(
        &self,
        article: &crate::freshrss::Article,
        summary: Option<String>,
        digest_date: String,
    ) -> anyhow::Result<()> {
        let payload = ArticlePayload {
            title: article.title.clone(),
            url: article.url.clone(),
            feed_title: article.feed_title.clone(),
            feed_url: article.feed_url.clone(),
            author: article.author.clone(),
            content: article.content.clone(),
            summary,
            published: article.published,
            digest_date,
        };
        let payload_json = serde_json::to_value(&payload)?;
        let payload: Payload = Payload::try_from(payload_json)
            .map_err(|e| anyhow::anyhow!("Failed to create payload: {e}"))?;
        let content_preview: String = article.content.trim().chars().take(500).collect();
        if article.content.trim().len() > 500 {
            log::warn!(
                "Article content truncated from {} to {} chars for embedding: {}",
                article.content.trim().len(),
                500,
                article.title
            );
        }
        let embedding = self
            .embed_text(&format!("{} {}", article.title, content_preview))
            .await?;

        self.qdrant
            .upsert_points(
                UpsertPointsBuilder::new(
                    &self.config.collection,
                    vec![PointStruct::new(article.id, embedding, payload)],
                )
                .wait(true),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upsert point: {e}"))?;
        log::debug!("Upserted article: {}", article.title);
        Ok(())
    }

    pub async fn retrieve_rag_context(
        &self,
        query: &str,
        top_k: usize,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let query_embedding = self.embed_text(query).await?;
        let response = self
            .qdrant
            .query(
                QueryPointsBuilder::new(&self.config.collection)
                    .query(query_embedding)
                    .limit(top_k as u64)
                    .with_payload(true)
                    .build(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to search: {e}"))?;
        let results = response
            .result
            .iter()
            .map(|r| serde_json::to_value(&r.payload).unwrap_or(serde_json::Value::Null))
            .collect();
        Ok(results)
    }

    pub async fn get_recent_articles(
        &self,
        days: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let since = chrono::Utc::now().timestamp() - (days * 86400);
        let response = self
            .qdrant
            .scroll(
                ScrollPointsBuilder::new(&self.config.collection)
                    .filter(Filter::must([Condition::range(
                        "published",
                        qdrant_client::qdrant::Range {
                            gt: Some(since as f64),
                            ..Default::default()
                        },
                    )]))
                    .with_payload(true)
                    .with_vectors(false)
                    .limit(limit as u32)
                    .order_by(qdrant_client::qdrant::OrderBy {
                        key: "published".to_string(),
                        direction: Some(0), // 0 = Asc, 1 = Desc
                        start_from: None,
                    })
                    .build(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to scroll: {e}"))?;
        let results = response
            .result
            .iter()
            .map(|r| serde_json::to_value(&r.payload).unwrap_or(serde_json::Value::Null))
            .collect();
        Ok(results)
    }

    pub async fn update_article_summaries(
        &self,
        url_summaries: &std::collections::HashMap<String, String>,
    ) -> anyhow::Result<()> {
        for (url, summary) in url_summaries {
            let payload: Payload = serde_json::json!({
                "summary": summary
            })
            .try_into()
            .map_err(|e| anyhow::anyhow!("Failed to create payload: {e}"))?;
            self.qdrant
                .set_payload(
                    SetPayloadPointsBuilder::new(&self.config.collection, payload)
                        .points_selector(Filter::must([Condition::matches("url", url.clone())]))
                        .build(),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to update summary for {}: {e}", url))?;
            log::debug!("Updated summary for article: {}", url);
        }
        Ok(())
    }
}
