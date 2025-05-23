use async_openai::config::OpenAIConfig;

#[derive(Debug)]
pub struct ModelConfig {
    pub trial: bool,
    endpoint: Option<String>,
    model: String,
    api_key: Option<String>,
}

// short-term trial key for testing
const DAVE_TRIAL: &str = unsafe {
    std::str::from_utf8_unchecked(&[
        0x73, 0x6b, 0x2d, 0x70, 0x72, 0x6f, 0x6a, 0x2d, 0x54, 0x6b, 0x61, 0x48, 0x46, 0x32, 0x73,
        0x72, 0x43, 0x59, 0x73, 0x5a, 0x62, 0x33, 0x6f, 0x6b, 0x43, 0x75, 0x61, 0x78, 0x39, 0x57,
        0x76, 0x72, 0x41, 0x46, 0x67, 0x5f, 0x39, 0x58, 0x78, 0x35, 0x65, 0x37, 0x4b, 0x53, 0x36,
        0x76, 0x32, 0x32, 0x51, 0x30, 0x67, 0x48, 0x61, 0x58, 0x6b, 0x67, 0x6e, 0x4e, 0x4d, 0x63,
        0x7a, 0x69, 0x72, 0x5f, 0x44, 0x57, 0x6e, 0x7a, 0x43, 0x77, 0x52, 0x50, 0x4e, 0x50, 0x39,
        0x6b, 0x5a, 0x79, 0x75, 0x57, 0x4c, 0x35, 0x54, 0x33, 0x42, 0x6c, 0x62, 0x6b, 0x46, 0x4a,
        0x72, 0x66, 0x49, 0x4b, 0x31, 0x77, 0x4f, 0x67, 0x31, 0x6a, 0x37, 0x54, 0x57, 0x42, 0x5a,
        0x67, 0x66, 0x49, 0x75, 0x30, 0x51, 0x48, 0x4e, 0x31, 0x70, 0x6a, 0x72, 0x37, 0x4b, 0x38,
        0x55, 0x54, 0x6d, 0x34, 0x50, 0x6f, 0x65, 0x47, 0x39, 0x61, 0x35, 0x79, 0x6c, 0x78, 0x45,
        0x4f, 0x6f, 0x74, 0x43, 0x47, 0x42, 0x36, 0x65, 0x7a, 0x59, 0x5a, 0x37, 0x70, 0x54, 0x38,
        0x63, 0x44, 0x75, 0x66, 0x75, 0x36, 0x52, 0x4d, 0x6b, 0x6c, 0x2d, 0x44, 0x51, 0x41,
    ])
};

impl Default for ModelConfig {
    fn default() -> Self {
        let api_key = std::env::var("DAVE_API_KEY")
            .ok()
            .or(std::env::var("OPENAI_API_KEY").ok());

        // trial mode?
        let trial = api_key.is_none();
        let api_key = api_key.or(Some(DAVE_TRIAL.to_string()));

        ModelConfig {
            trial,
            endpoint: std::env::var("DAVE_ENDPOINT").ok(),
            model: std::env::var("DAVE_MODEL")
                .ok()
                .unwrap_or("gpt-4o".to_string()),
            api_key,
        }
    }
}

impl ModelConfig {
    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn ollama() -> Self {
        ModelConfig {
            trial: false,
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
