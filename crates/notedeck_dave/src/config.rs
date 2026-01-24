use async_openai::config::OpenAIConfig;

/// Available AI providers for Dave
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AiProvider {
    #[default]
    OpenAI,
    Anthropic,
    Ollama,
}

impl AiProvider {
    pub const ALL: [AiProvider; 3] = [
        AiProvider::OpenAI,
        AiProvider::Anthropic,
        AiProvider::Ollama,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            AiProvider::OpenAI => "OpenAI",
            AiProvider::Anthropic => "Anthropic",
            AiProvider::Ollama => "Ollama",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            AiProvider::OpenAI => "gpt-4o",
            AiProvider::Anthropic => "claude-sonnet-4-20250514",
            AiProvider::Ollama => "hhao/qwen2.5-coder-tools:latest",
        }
    }

    pub fn default_endpoint(&self) -> Option<&'static str> {
        match self {
            AiProvider::OpenAI => None,
            AiProvider::Anthropic => Some("https://api.anthropic.com/v1"),
            AiProvider::Ollama => Some("http://localhost:11434/v1"),
        }
    }

    pub fn requires_api_key(&self) -> bool {
        match self {
            AiProvider::OpenAI | AiProvider::Anthropic => true,
            AiProvider::Ollama => false,
        }
    }

    pub fn available_models(&self) -> &'static [&'static str] {
        match self {
            AiProvider::OpenAI => &["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-3.5-turbo"],
            AiProvider::Anthropic => &[
                "claude-sonnet-4-20250514",
                "claude-opus-4-20250514",
                "claude-3-5-sonnet-20241022",
                "claude-3-5-haiku-20241022",
            ],
            AiProvider::Ollama => &[
                "hhao/qwen2.5-coder-tools:latest",
                "llama3.2:latest",
                "mistral:latest",
                "codellama:latest",
            ],
        }
    }
}

/// User-configurable settings for Dave AI
#[derive(Debug, Clone)]
pub struct DaveSettings {
    pub provider: AiProvider,
    pub model: String,
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
}

impl Default for DaveSettings {
    fn default() -> Self {
        DaveSettings {
            provider: AiProvider::default(),
            model: AiProvider::default().default_model().to_string(),
            endpoint: None,
            api_key: None,
        }
    }
}

impl DaveSettings {
    /// Create settings with provider defaults applied
    pub fn with_provider(provider: AiProvider) -> Self {
        DaveSettings {
            provider,
            model: provider.default_model().to_string(),
            endpoint: provider.default_endpoint().map(|s| s.to_string()),
            api_key: None,
        }
    }

    /// Create settings from an existing ModelConfig (preserves env var values)
    pub fn from_model_config(config: &ModelConfig) -> Self {
        let provider = match config.backend {
            BackendType::OpenAI => AiProvider::OpenAI,
            BackendType::Claude => AiProvider::Anthropic,
        };

        let api_key = match provider {
            AiProvider::Anthropic => config.anthropic_api_key.clone(),
            _ => config.api_key().map(|s| s.to_string()),
        };

        DaveSettings {
            provider,
            model: config.model().to_string(),
            endpoint: config
                .endpoint()
                .map(|s| s.to_string())
                .or_else(|| provider.default_endpoint().map(|s| s.to_string())),
            api_key,
        }
    }
}

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

    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub fn ollama() -> Self {
        ModelConfig {
            trial: false,
            endpoint: std::env::var("OLLAMA_HOST").ok().map(|h| h + "/v1"),
            model: "hhao/qwen2.5-coder-tools:latest".to_string(),
            api_key: None,
        }
    }

    /// Create a ModelConfig from DaveSettings
    pub fn from_settings(settings: &DaveSettings) -> Self {
        // If settings have an API key, we're not in trial mode
        // For Ollama, trial is always false since no key is required
        let trial = settings.provider.requires_api_key() && settings.api_key.is_none();

        let backend = match settings.provider {
            AiProvider::OpenAI | AiProvider::Ollama => BackendType::OpenAI,
            AiProvider::Anthropic => BackendType::Claude,
        };

        let anthropic_api_key = if settings.provider == AiProvider::Anthropic {
            settings.api_key.clone()
        } else {
            None
        };

        let api_key = if settings.provider != AiProvider::Anthropic {
            settings.api_key.clone()
        } else {
            None
        };

        ModelConfig {
            trial,
            backend,
            endpoint: settings.endpoint.clone(),
            model: settings.model.clone(),
            api_key,
            anthropic_api_key,
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
