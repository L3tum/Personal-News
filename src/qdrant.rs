use anyhow;
use chrono::DateTime;
use chrono::Utc;
use qdrant_client::prelude::*;
use qdrant_client::qdrant::points_struct::{
    CreatePoints, Filter, GeoBboxQuery, GeoBoundingBox, GeoPoint, GeoRadius, HasIdCondition,
    IdRange, MatchAny, MatchValue, PayloadIncludeSelector, PointStruct,
};
use qdrant_client::qdrant::payload_schema::DataType;
use qdrant_client::qdrant::vectors::VectorsObject;
use qdrant_client::qdrant::vectors::Vectors;
use qdrant_client::qdrant::{Value, Payload};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

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
    pub published: i64, // Unix timestamp
    pub digest_date: String, // date of the digest when this was included
}

pub struct QdrantClientWrapper {
    client: QdrantClient,
    config: QdrantConfig,
}

impl QdrantClientWrapper {
    pub async fn new(config: QdrantConfig) -> anyhow::Result<Self> {
        let client = if let Some(api_key) = &config.api_key {
            QdrantClient::new(Some(&config.url), Some(api_key))
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to Qdrant: {}", e))?
        } else {
            QdrantClient::new(Some(&config.url), None)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to Qdrant: {}", e))?
        };
        
        // Ensure the collection exists
        let wrapper = Self {
            client,
            config,
        };
        
        wrapper.ensure_collection().await?;
        
        Ok(wrapper)
    }

    async fn ensure_collection(&self) -> anyhow::Result<()> {
        // Check if collection exists
        let collections = self.client
            .list_collections()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list collections: {}", e))?;
        
        let exists = collections.collections.iter()
            .any(|c| c.name == self.config.collection);
        
        if !exists {
            self.client
                .create_collection(&CreateCollection {
                    collection_name: self.config.collection.clone(),
                    vectors_config: Some(vectors_config::Config::Params(VectorParams {
                        size: 768, // standard for nomic-embed-text
                        distance: Distance::Cosine,
                        on_disk: true,
                        quantization_config: None,
                    })),
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create collection: {}", e))?;
            
            log::info!("Created collection: {}", self.config.collection);
        }
        
        Ok(())
    }

    /// Generate an embedding using Ollama (simple approach)
    pub async fn embed_text(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        // Use reqwest to call Ollama embedding endpoint
        let client = reqwest::Client::new();
        let url = format!("{}/api/embed", std::env::var("OLLAMA_URL").unwrap_or("http://localhost:11434".to_string()));
        
        let response = client.post(&url)
            .json(&serde_json::json!({
                "model": self.config.embedding_model,
                "input": text,
            }))
            .send()
            .await?;
        
        let body: serde_json::Value = response.json().await?;
        let embedding = body["embeddings"][0]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Unexpected embedding format"))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();
        
        Ok(embedding)
    }

    /// Upsert an article (insert or update if same URL exists)
    pub async fn upsert_article(
        &self,
        article: &crate::freshrss::Article,
        summary: Option<String>,
        digest_date: String,
    ) -> anyhow::Result<()> {
        // First, check if this article already exists by URL
        let existing = self.search_by_url(&article.url).await?;
        
        if existing.is_some() {
            // Already exists, skip
            log::debug!("Article already exists: {}", article.url);
            return Ok(());
        }
        
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
        
        let embedding = self.embed_text(&format!("{} {}", article.title, article.content.trim().chars().take(500).collect::<String>())).await?;
        
        let payload_map = serde_json::to_value(&payload)?;
        let point_id = article.id.to_string();
        
        self.client
            .upsert_points(&UpsertPoints {
                collection_name: self.config.collection.clone(),
                wait: true,
                points: Some(Points {
                    points: vec![PointStruct {
                        id: Some(qdrant_client::qdrant::PointId::NumId(
                            article.id,
                        )),
                        vectors: Some(qdrant_client::qdrant::vectors::VectorsObject::Vector(
                            qdrant_client::qdrant::Vector {
                                data: embedding,
                            },
                        )),
                        payload: Payload(payload_map.as_object()
                            .expect("payload should be object")
                            .iter()
                            .map(|(k, v)| (k.clone(), PayloadSchemaType::from(v)))
                            .collect(),
                        )
                        .unwrap_or_default(),
                    }],
                }),
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upsert point: {}", e))?;
        
        log::debug!("Upserted article: {}", article.title);
        Ok(())
    }

    /// Search by URL to avoid duplicates
    async fn search_by_url(&self, url: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let payload_filter = PayloadFilter {
            must: vec![Condition::Field(FieldCondition {
                key: "url".to_string(),
                r#match: Some(qdrant_client::qdrant::r#match::MatchValue::Keyword(
                    url.to_string(),
                )),
                ..Default::default()
            })],
            must_not: vec![],
            should: vec![],
            filter: None,
        };
        
        let response = self.client
            .scroll_points(&ScrollPoints {
                collection_name: self.config.collection.clone(),
                limit: 1,
                offset: None,
                with_payload: Some(PayloadIncludeSelector {
                    inclusion: Some(qdrant_client::qdrant::payload_include_selector::Inclusion::Keys(
                        vec!["title".to_string(), "url".to_string(), "summary".to_string()],
                    )),
                }),
                filter: Some(Filter {
                    must: vec![Condition::Field(FieldCondition {
                        key: "url".to_string(),
                        r#match: Some(qdrant_client::qdrant::r#match::MatchValue::Keyword(
                            url.to_string(),
                        )),
                        ..Default::default()
                    })],
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to scroll: {}", e))?;
        
        if response.result.points.is_empty() {
            Ok(None)
        } else {
            // Convert payload to JSON value
            let point = &response.result.points[0];
            let payload = point.payload.clone();
            Ok(Some(serde_json::to_value(&payload)?))
        }
    }

    /// Retrieve contextually similar articles using RAG
    pub async fn retrieve_rag_context(
        &self,
        query: &str,
        top_k: usize,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let query_embedding = self.embed_text(query).await?;
        
        let response = self.client
            .search_points(&SearchPoints {
                collection_name: self.config.collection.clone(),
                vector: query_embedding,
                top: top_k as u64,
                filter: None,
                with_payload: Some(PayloadIncludeSelector {
                    inclusion: Some(qdrant_client::qdrant::payload_include_selector::Inclusion::Keys(
                        vec![
                            "title".to_string(),
                            "summary".to_string(),
                            "digest_date".to_string(),
                            "url".to_string(),
                            "feed_title".to_string(),
                        ],
                    )),
                }),
                with_vector: false,
                score_threshold: None,
                params: None,
                consistent: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to search: {}", e))?;
        
        let results = response.result
            .iter()
            .map(|result| {
                serde_json::to_value(&result.payload)
                    .unwrap_or(serde_json::Value::Null)
            })
            .collect();
        
        Ok(results)
    }

    /// Retrieve articles from a specific date range (for daily context)
    pub async fn get_recent_articles(
        &self,
        days: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let since = Utc::now().timestamp() - (days * 86400);
        
        let response = self.client
            .scroll_points(&ScrollPoints {
                collection_name: self.config.collection.clone(),
                limit: limit as u64,
                offset: None,
                with_payload: Some(PayloadIncludeSelector {
                    inclusion: Some(qdrant_client::qdrant::payload_include_selector::Inclusion::Keys(
                        vec![
                            "title".to_string(),
                            "summary".to_string(),
                            "digest_date".to_string(),
                            "url".to_string(),
                            "feed_title".to_string(),
                        ],
                    )),
                }),
                filter: Some(Filter {
                    must: vec![Condition::Field(FieldCondition {
                        key: "published".to_string(),
                        range: Some(qdrant_client::qdrant::Range {
                            gt: Some(since),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })],
                    ..Default::default()
                }),
                order_by: Some(OrderBy {
                    key: "published".to_string(),
                    direction: Some(qdrant_client::qdrant::order_by::Direction::Desc as i32),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to scroll: {}", e))?;
        
        let results = response.result
            .iter()
            .map(|result| {
                serde_json::to_value(&result.payload)
                    .unwrap_or(serde_json::Value::Null)
            })
            .collect();
        
        Ok(results)
    }
}

// Helper function to convert JSON Value to Qdrant Payload
fn PayloadSchemaType(value: &serde_json::Value) -> Payload {
    match value {
        serde_json::Value::String(s) => Payload::KeywordPayload(s.clone()),
        serde_json::Value::Number(n) => Payload::IntegerPayload(
            n.as_i64().unwrap_or(0),
        ),
        serde_json::Value::Bool(b) => Payload::IntegerPayload(*b as i64),
        serde_json::Value::Null => Payload::KeywordPayload("".to_string()),
        _ => Payload::KeywordPayload(
            serde_json::to_string(value).unwrap_or_default(),
        ),
    }
}
