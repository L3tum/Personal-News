use reqwest::Client;

pub struct LibreTranslateClient {
    base_url: String,
    api_key: Option<String>,
    http: Client,
}

impl LibreTranslateClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            base_url,
            api_key,
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to create HTTP client for translation"),
        }
    }

    /// Detect the language of a given text using LibreTranslate's /detection endpoint.
    /// Returns the detected language code (e.g., "en", "de", "fr").
    pub async fn detect_language(&self, text: &str) -> anyhow::Result<String> {
        let url = format!("{}/v2/detect", self.base_url);
        let body = serde_json::json!({
            "q": text,
        });

        let mut request = self
            .http
            .post(&url)
            .json(&body)
            .header("Content-Type", "application/json");

        if let Some(ref api_key) = self.api_key {
            request = request.header("Authorization", api_key);
        }

        let response = request.send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "LibreTranslate detection error: {} - {}",
                status,
                body
            ));
        }

        let result: serde_json::Value = response.json().await?;
        let lang_code = result["0"]["language"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Unexpected detection response format"))?
            .to_string();
        Ok(lang_code)
    }

    /// Translate text from source language to target language.
    /// source_lang: detected language code (e.g., "en")
    /// target_lang: desired target language code (e.g., "de")
    /// Returns the translated text.
    pub async fn translate(
        &self,
        text: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> anyhow::Result<String> {
        let url = format!("{}/v2/translate", self.base_url);
        let body = serde_json::json!({
            "q": text,
            "source": source_lang,
            "target": target_lang,
        });

        let mut request = self
            .http
            .post(&url)
            .json(&body)
            .header("Content-Type", "application/json");

        if let Some(ref api_key) = self.api_key {
            request = request.header("Authorization", api_key);
        }

        let response = request.send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "LibreTranslate translation error: {} - {}",
                status,
                body
            ));
        }

        let result: serde_json::Value = response.json().await?;
        let translated_text = result["translatedText"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Unexpected translation response format"))?
            .to_string();
        Ok(translated_text)
    }
}
