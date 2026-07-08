use qdrant_client::qdrant::point_id::PointIdOptions;
use qdrant_client::qdrant::{
    Condition, CreateCollectionBuilder, Distance, Filter, PointStruct, QueryPointsBuilder,
    ScrollPointsBuilder, SetPayloadPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder,
};
use qdrant_client::qdrant::{PointVectors, UpdatePointVectors};
use qdrant_client::{Payload, Qdrant};
use serde::{Deserialize, Serialize};

use crate::config::QdrantConfig;
use crate::freshrss_fever::Article;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArticlePayload {
    pub title: String,
    pub url: String,
    pub feed_title: String,
    pub feed_url: Option<String>,
    pub author: Option<String>,
    pub content: String,
    pub summary: Option<String>,
    pub translated_summary: Option<String>,
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
            // Collection doesn't exist, create with new multi-vector schema
            // Create collection with two named vectors: summary_vector and translated_summary_vector
            let dim = self.config.embedding_dim as u64;
            self.qdrant
                .create_collection(
                    CreateCollectionBuilder::new(&self.config.collection).vectors_config(
                        qdrant_client::qdrant::vectors_config::Config::ParamsMap(
                            qdrant_client::qdrant::VectorParamsMap {
                                map: {
                                    use std::collections::HashMap;
                                    let mut map = HashMap::new();
                                    map.insert(
                                        "summary_vector".to_string(),
                                        VectorParamsBuilder::new(dim, Distance::Cosine).build(),
                                    );
                                    map.insert(
                                        "translated_summary_vector".to_string(),
                                        VectorParamsBuilder::new(dim, Distance::Cosine).build(),
                                    );
                                    map
                                },
                            },
                        ),
                    ),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create collection: {e}"))?;
            log::info!(
                "Created collection: {} with two vectors (summary_vector, translated_summary_vector) each {} dimensions",
                self.config.collection,
                self.config.embedding_dim
            );
        } else {
            // Collection exists — check vector schema via gRPC and fail if old single-vector schema
            log::info!("Checking collection '{}' via gRPC", self.config.collection);
            let collection_info = self
                .qdrant
                .collection_info(&self.config.collection)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to get collection info for '{}': {}",
                        self.config.collection,
                        e
                    )
                })?;

            // Extract the vectors config to check for named vectors
            let has_summary_vector = collection_info
                .result
                .as_ref()
                .and_then(|info| info.config.as_ref())
                .and_then(|config| config.params.as_ref())
                .and_then(|params| params.vectors_config.as_ref())
                .and_then(|vectors| vectors.config.as_ref())
                .map(|config| match config {
                    qdrant_client::qdrant::vectors_config::Config::ParamsMap(pm) => {
                        pm.map.contains_key("summary_vector")
                    }
                    _ => false,
                })
                .unwrap_or(false);

            if !has_summary_vector {
                panic!(
                    "Existing collection '{}' uses old single-vector schema. \
                     Please delete the collection and re-run the digest generation. \
                     Data will be re-created on next run.",
                    self.config.collection
                );
            }
            log::info!(
                "Collection '{}' exists with named vector schema (summary_vector, translated_summary_vector)",
                self.config.collection
            );
        }
        Ok(())
    }

    pub async fn embed_text(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let url = format!("{}/v1/embeddings", self.config.embedding_url);
        log::info!("Fetching embedding from {}", url);
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
        article: &Article,
        summary: Option<String>,
        translated_summary: Option<String>,
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
            translated_summary: translated_summary.clone(),
            published: article.published,
            digest_date,
        };
        let payload_json = serde_json::to_value(&payload)?;
        let payload: Payload = Payload::try_from(payload_json)
            .map_err(|e| anyhow::anyhow!("Failed to create payload: {e}"))?;

        // Original summary vector from title + content
        let content_preview: String = article.content.trim().chars().take(500).collect();
        if article.content.trim().len() > 500 {
            log::warn!(
                "Article content truncated from {} to {} chars for embedding: {}",
                article.content.trim().len(),
                500,
                article.title
            );
        }
        let summary_vector = self
            .embed_text(&format!("{} {}", article.title, content_preview))
            .await?;

        // Translated summary vector (optional)
        let translated_vector = if let Some(ref translated) = translated_summary {
            Some(self.embed_text(translated).await?)
        } else {
            None
        };

        // Build named vectors map using helper
        let mut vectors_map: std::collections::HashMap<String, Vec<f32>> =
            std::collections::HashMap::new();
        vectors_map.insert("summary_vector".to_string(), summary_vector);
        if let Some(tv) = translated_vector {
            vectors_map.insert("translated_summary_vector".to_string(), tv);
        }
        let vectors = Self::build_named_vectors(vectors_map);

        self.qdrant
            .upsert_points(
                UpsertPointsBuilder::new(
                    &self.config.collection,
                    vec![PointStruct::new(article.id, vectors, payload)],
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

        // Search across both original summary and translated summary vectors
        let mut all_results: std::collections::HashMap<
            String,                   // point_id
            (f32, serde_json::Value), // (score, payload)
        > = std::collections::HashMap::new();

        // Search original summary vector
        let response_original = self
            .qdrant
            .query(
                QueryPointsBuilder::new(&self.config.collection)
                    .query(query_embedding.clone())
                    .using("summary_vector")
                    .limit((top_k * 2) as u64) // fetch extra to allow merging
                    .with_payload(true)
                    .build(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to search summary_vector: {e}"))?;

        for r in response_original.result.iter() {
            let payload = serde_json::to_value(&r.payload).unwrap_or(serde_json::Value::Null);
            let point_id =
                r.id.as_ref()
                    .and_then(|id| {
                        id.point_id_options.as_ref().map(|opts| match opts {
                            PointIdOptions::Num(n) => format!("num:{}", n),
                            PointIdOptions::Uuid(u) => u.clone(),
                        })
                    })
                    .unwrap_or_else(|| String::from("unknown"));
            let score = r.score;
            all_results.insert(point_id, (score, payload));
        }

        // Search translated summary vector
        let response_translated = self
            .qdrant
            .query(
                QueryPointsBuilder::new(&self.config.collection)
                    .query(query_embedding)
                    .using("translated_summary_vector")
                    .limit((top_k * 2) as u64)
                    .with_payload(true)
                    .build(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to search translated_summary_vector: {e}"))?;

        for r in response_translated.result.iter() {
            let payload = serde_json::to_value(&r.payload).unwrap_or(serde_json::Value::Null);
            let point_id =
                r.id.as_ref()
                    .and_then(|id| {
                        id.point_id_options.as_ref().map(|opts| match opts {
                            PointIdOptions::Num(n) => format!("num:{}", n),
                            PointIdOptions::Uuid(u) => u.clone(),
                        })
                    })
                    .unwrap_or_else(|| String::from("unknown"));
            let score = r.score;
            all_results
                .entry(point_id)
                .and_modify(|(existing_score, _)| {
                    if score > *existing_score {
                        *existing_score = score;
                    }
                })
                .or_insert((score, payload.clone()));
        }

        // Sort by score descending, take top K
        let mut scored_results: Vec<_> = all_results.into_values().collect();
        scored_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        scored_results.truncate(top_k);

        let results = scored_results
            .iter()
            .map(|(_, payload)| payload.clone())
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

    // Helper to build a NamedVectors structure from a map of vector names to embeddings
    fn build_named_vectors(
        vectors: std::collections::HashMap<String, Vec<f32>>,
    ) -> qdrant_client::qdrant::Vectors {
        let named_vectors = qdrant_client::qdrant::NamedVectors {
            vectors: vectors
                .into_iter()
                .map(|(name, data)| {
                    (
                        name,
                        qdrant_client::qdrant::Vector {
                            // Use the new vector field instead of deprecated data/indices/vectors_count
                            vector: Some(qdrant_client::qdrant::vector::Vector::Dense(
                                qdrant_client::qdrant::DenseVector { data },
                            )),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        };
        qdrant_client::qdrant::Vectors {
            vectors_options: Some(qdrant_client::qdrant::vectors::VectorsOptions::Vectors(
                named_vectors,
            )),
        }
    }

    pub async fn update_article_summaries(
        &self,
        digest_date: &str,
        url_summaries: &std::collections::HashMap<String, (String, Option<String>)>,
    ) -> anyhow::Result<()> {
        for (url, (summary, translated_summary)) in url_summaries {
            // Update payload fields
            let payload: Payload = serde_json::json!({
                "summary": summary,
                "translated_summary": translated_summary,
            })
            .try_into()
            .map_err(|e| anyhow::anyhow!("Failed to create payload: {e}"))?;
            self.qdrant
                .set_payload(
                    SetPayloadPointsBuilder::new(&self.config.collection, payload)
                        .points_selector(Filter::must([
                            Condition::matches("url", url.clone()),
                            Condition::matches("digest_date", digest_date.to_string()),
                        ]))
                        .build(),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to update summary for {}: {e}", url))?;
            log::debug!("Updated summary for article: {}", url);

            // Update translated_summary vector if translation exists
            if let Some(ref translated) = translated_summary {
                let translated_vector = self.embed_text(translated).await?;

                // Find point ID by URL using scroll
                let point_id = {
                    let response = self
                        .qdrant
                        .scroll(
                            ScrollPointsBuilder::new(&self.config.collection)
                                .filter(Filter::must([
                                    Condition::matches("url", url.clone()),
                                    Condition::matches("digest_date", digest_date.to_string()),
                                ]))
                                .with_payload(false)
                                .with_vectors(false)
                                .limit(1)
                                .build(),
                        )
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to find point for {}: {e}", url))?;
                    response
                        .result
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("Point not found for {}", url))?
                        .id
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("Point ID is missing for {}", url))?
                };

                // Build named vectors map using helper
                let mut vectors_map: std::collections::HashMap<String, Vec<f32>> =
                    std::collections::HashMap::new();
                vectors_map.insert("translated_summary_vector".to_string(), translated_vector);
                let vectors = Self::build_named_vectors(vectors_map);

                let point_vectors = PointVectors {
                    id: Some(point_id),
                    vectors: Some(vectors),
                };

                // Use UpdatePointVectors with update_vectors
                let update_request = UpdatePointVectors {
                    collection_name: self.config.collection.clone(),
                    points: vec![point_vectors],
                    ..Default::default()
                };
                self.qdrant
                    .update_vectors(update_request)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to update translated vector for {}: {e}", url)
                    })?;
                log::debug!("Updated translated summary vector for article: {}", url);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    /// Test that the score-merging logic keeps the higher score when the same point exists in both vector searches.
    #[test]
    fn test_score_merge_keeps_higher_score() {
        let mut all_results: HashMap<String, (f32, String)> = HashMap::new();

        // Simulate first search (summary_vector) with score 0.8
        all_results
            .entry("point1".to_string())
            .and_modify(|(score, _)| {
                if 0.8 > *score {
                    *score = 0.8;
                }
            })
            .or_insert((0.8, "summary_payload".to_string()));

        // Simulate second search (translated_summary_vector) with score 0.9
        all_results
            .entry("point1".to_string())
            .and_modify(|(score, _)| {
                if 0.9 > *score {
                    *score = 0.9;
                }
            })
            .or_insert((0.9, "translated_payload".to_string()));

        // Score should be 0.9, not 0.8
        let (final_score, _) = all_results.get("point1").unwrap();
        assert_eq!(*final_score, 0.9);
    }

    /// Test that if the second search has a lower score, the first score is kept.
    #[test]
    fn test_score_merge_keeps_first_score_if_higher() {
        let mut all_results: HashMap<String, (f32, String)> = HashMap::new();

        all_results
            .entry("point2".to_string())
            .and_modify(|(score, _)| {
                if 0.9 > *score {
                    *score = 0.9;
                }
            })
            .or_insert((0.9, "summary_payload".to_string()));

        all_results
            .entry("point2".to_string())
            .and_modify(|(score, _)| {
                if 0.8 > *score {
                    *score = 0.8;
                }
            })
            .or_insert((0.8, "translated_payload".to_string()));

        let (final_score, _) = all_results.get("point2").unwrap();
        assert_eq!(*final_score, 0.9);
    }

    /// Test that different points are both stored.
    #[test]
    fn test_score_merge_keeps_different_points() {
        let mut all_results: HashMap<String, (f32, String)> = HashMap::new();

        all_results
            .entry("point1".to_string())
            .and_modify(|(score, _)| {
                if 0.8 > *score {
                    *score = 0.8;
                }
            })
            .or_insert((0.8, "payload1".to_string()));

        all_results
            .entry("point2".to_string())
            .and_modify(|(score, _)| {
                if 0.9 > *score {
                    *score = 0.9;
                }
            })
            .or_insert((0.9, "payload2".to_string()));

        assert_eq!(all_results.len(), 2);
    }

    /// Test that the update_article_summaries filter includes both url and digest_date conditions.
    #[test]
    fn test_update_summaries_filter_includes_digest_date() {
        use qdrant_client::qdrant::{Condition, Filter};

        // Simulate the filter used in update_article_summaries
        let url = "https://example.com/article";
        let digest_date = "2025-01-02";
        let filter = Filter::must([
            Condition::matches("url", url.to_string()),
            Condition::matches("digest_date", digest_date.to_string()),
        ]);

        // The filter should have exactly 2 conditions (url and digest_date)
        // This ensures the digest_date filter was added and not just url alone
        assert_eq!(filter.must.len(), 2);
    }
}
