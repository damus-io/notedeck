use async_openai::config::OpenAIConfig;

#[derive(Debug)]
pub struct ModelConfig {
    endpoint: Option<String>,
    model: String,
    api_key: Option<String>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig {
            endpoint: None,
            model: "gpt-4o".to_string(),
            api_key: std::env::var("OPENAI_API_KEY").ok(),
        }
    }
}

impl ModelConfig {
    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn ollama() -> Self {
        ModelConfig {
            endpoint: std::env::var("OLLAMA_HOST").ok().map(|h| h + "/v1"),
            model: "hhao/qwen2.5-coder-tools:latest".to_string(),
            api_key: None,
        }
    }

    pub fn to_api(&self) -> OpenAIConfig {
        let mut cfg = OpenAIConfig::new();
        if let Some(endpoint) = &self.endpoint {
            cfg = cfg.with_api_base(endpoint.to_owned());
        }

        if let Some(api_key) = &self.api_key {
            cfg = cfg.with_api_key(api_key.to_owned());
        }

        cfg
    }
}
