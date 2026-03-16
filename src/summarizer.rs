use crate::formatting;
use std::sync::Arc;
use tokio::sync::RwLock;

/// LLM-backed summarizer for tool actions that fall outside rule-based patterns.
/// Uses a configurable endpoint (Anthropic API, Z.AI proxy, etc.) with Haiku
/// for fast, cheap natural-language summaries.
pub struct LlmSummarizer {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    enabled: bool,
    /// Simple cache: tool_name+input_hash -> summary
    cache: Arc<RwLock<std::collections::HashMap<String, String>>>,
}

impl LlmSummarizer {
    /// Create a new LLM summarizer.
    /// - `endpoint`: API endpoint URL (e.g., "https://api.anthropic.com/v1/messages"
    ///   or "http://localhost:9600/chat" for Z.AI proxy)
    /// - `api_key`: Optional API key for authentication
    pub fn new(endpoint: Option<String>, api_key: Option<String>) -> Self {
        let enabled = endpoint.is_some();
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            endpoint: endpoint.unwrap_or_default(),
            api_key,
            enabled,
            cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Summarize a tool action. Uses rule-based first, falls back to LLM
    /// only when the rule-based result is generic.
    pub async fn summarize(&self, tool: &str, input: Option<&serde_json::Value>) -> String {
        let rule_based = formatting::summarize_tool_action(tool, input);

        // Only call LLM if the rule-based summary is generic
        if !self.enabled || !is_generic_summary(&rule_based) {
            return rule_based;
        }

        // Build cache key from tool + input hash
        let cache_key = build_cache_key(tool, input);
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }
        }

        // Call LLM for a better summary
        match self.call_llm(tool, input).await {
            Ok(summary) => {
                let mut cache = self.cache.write().await;
                // Cap cache at 200 entries
                if cache.len() > 200 {
                    cache.clear();
                }
                cache.insert(cache_key, summary.clone());
                summary
            }
            Err(e) => {
                tracing::debug!(error = %e, "LLM summarize fallback failed, using rule-based");
                rule_based
            }
        }
    }

    /// Call the LLM to summarize a tool action
    async fn call_llm(
        &self,
        tool: &str,
        input: Option<&serde_json::Value>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let input_str = input
            .map(|v| serde_json::to_string(v).unwrap_or_default())
            .unwrap_or_default();

        // Truncate input to keep prompt small
        let truncated_input: String = input_str.chars().take(500).collect();

        let prompt = format!(
            "Summarize this Claude Code tool action in 3-8 words. \
             Be conversational and human-readable, like a status update. \
             No emoji. No markdown. No quotes. Just the summary.\n\n\
             Tool: {}\nInput: {}",
            tool, truncated_input
        );

        // Try Anthropic Messages API format first
        if self.endpoint.contains("anthropic") || self.endpoint.contains("messages") {
            return self.call_anthropic(&prompt).await;
        }

        // Z.AI / generic chat endpoint
        self.call_generic_chat(&prompt).await
    }

    /// Call Anthropic Messages API
    async fn call_anthropic(
        &self,
        prompt: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let body = serde_json::json!({
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 30,
            "messages": [{
                "role": "user",
                "content": prompt
            }]
        });

        let mut req = self.client.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            req = req
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01");
        }

        let resp = req.send().await?;
        let json: serde_json::Value = resp.json().await?;

        let text = json
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|m| m.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if text.is_empty() {
            Err("Empty LLM response".into())
        } else {
            Ok(text)
        }
    }

    /// Call Z.AI or other generic chat endpoint
    async fn call_generic_chat(
        &self,
        prompt: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let body = serde_json::json!({
            "prompt": prompt,
            "timeout": 4000
        });

        let resp = self.client.post(&self.endpoint).json(&body).send().await?;
        let json: serde_json::Value = resp.json().await?;

        let text = json
            .get("response")
            .or_else(|| json.get("text"))
            .or_else(|| json.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if text.is_empty() {
            Err("Empty chat response".into())
        } else {
            Ok(text)
        }
    }
}

/// Check if a summary is generic (rule-based fallback)
fn is_generic_summary(summary: &str) -> bool {
    summary.starts_with("Using ") || summary.starts_with("Running `")
}

/// Build a cache key from tool name and input
fn build_cache_key(tool: &str, input: Option<&serde_json::Value>) -> String {
    let input_hash = input
        .map(|v| {
            use std::hash::{Hash, Hasher};
            let s = v.to_string();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            s.hash(&mut hasher);
            hasher.finish()
        })
        .unwrap_or(0);
    format!("{}:{}", tool, input_hash)
}
