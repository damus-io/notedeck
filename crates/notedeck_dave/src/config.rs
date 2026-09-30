use crate::backend::BackendType;
use async_openai::config::OpenAIConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;

// The run-config protocol type + event kind now live in `agentium-core`; keep
// them reachable as `crate::config::{RunConfig, AI_RUN_CONFIG_KIND}`.
pub use agentium_core::config::{RunConfig, AI_RUN_CONFIG_KIND};

/// Check if a binary exists on the system PATH.
pub fn has_binary_on_path(binary: &str) -> bool {
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|dir| dir.join(binary).is_file()))
        .unwrap_or(false)
        || env::var_os("PATH")
            .map(|paths| {
                env::split_paths(&paths).any(|dir| dir.join(format!("{}.exe", binary)).is_file())
            })
            .unwrap_or(false)
}

/// Detect which agentic backends are available based on binaries in PATH.
pub fn available_agentic_backends() -> Vec<BackendType> {
    let mut backends = Vec::new();
    if has_binary_on_path("claude") {
        backends.push(BackendType::Claude);
    }
    if has_binary_on_path("codex") {
        backends.push(BackendType::Codex);
    }
    backends
}

/// AI interaction mode - determines UI complexity and feature set
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiMode {
    /// Simple chat interface (OpenAI-style) - no permissions, no CWD, no scene view
    Chat,
    /// Full IDE with permissions, sessions, scene view, etc. (Claude backend)
    Agentic,
}

/// Available AI providers for Dave
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AiProvider {
    #[default]
    OpenAI,
    Anthropic,
    Ollama,
    Codex,
}

impl AiProvider {
    pub const ALL: [AiProvider; 4] = [
        AiProvider::OpenAI,
        AiProvider::Anthropic,
        AiProvider::Ollama,
        AiProvider::Codex,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            AiProvider::OpenAI => "OpenAI",
            AiProvider::Anthropic => "Anthropic",
            AiProvider::Ollama => "Ollama",
            AiProvider::Codex => "Codex",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            AiProvider::OpenAI => "gpt-5.2",
            AiProvider::Anthropic => "claude-sonnet-4-20250514",
            AiProvider::Ollama => "hhao/qwen2.5-coder-tools:latest",
            AiProvider::Codex => "gpt-5.3-codex",
        }
    }

    pub fn default_endpoint(&self) -> Option<&'static str> {
        match self {
            AiProvider::OpenAI | AiProvider::Codex => None,
            AiProvider::Anthropic => Some("https://api.anthropic.com/v1"),
            AiProvider::Ollama => Some("http://localhost:11434/v1"),
        }
    }

    pub fn requires_api_key(&self) -> bool {
        match self {
            AiProvider::OpenAI | AiProvider::Anthropic => true,
            AiProvider::Ollama | AiProvider::Codex => false,
        }
    }

    pub fn available_models(&self) -> &'static [&'static str] {
        match self {
            AiProvider::OpenAI => &["gpt-5.2"],
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
            AiProvider::Codex => &[
                "gpt-5.3-codex",
                "gpt-5.2-codex",
                "gpt-5-codex",
                "gpt-5-codex-mini",
                "codex-mini-latest",
            ],
        }
    }
}

/// User-configurable settings for Dave AI
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaveSettings {
    pub provider: AiProvider,
    pub model: String,
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    /// The key that opens a keyboard chord over the chat's collapsible blocks.
    /// Defaulted so settings files written before it existed still load.
    #[serde(default)]
    pub leader_key: LeaderKey,
    /// Environment variables exported into every agent session this host
    /// spawns, on every backend (e.g. `HEADWAY_COMMENT_NSEC_FILE`, so agent
    /// comments sign with the agent's own key in any worktree without touching
    /// the user's own shells). The session's `AGENTIUM_*` identity variables
    /// win over a same-named entry (see `backend::shared::session_env`).
    /// Edited in the settings panel's "Session environment" section. Defaulted
    /// so older settings files still load.
    #[serde(default)]
    pub session_env: BTreeMap<String, String>,
}

impl Default for DaveSettings {
    fn default() -> Self {
        DaveSettings {
            provider: AiProvider::default(),
            model: AiProvider::default().default_model().to_string(),
            endpoint: None,
            api_key: None,
            leader_key: LeaderKey::default(),
            session_env: BTreeMap::new(),
        }
    }
}

/// A user-configurable leader key: modifiers plus one key, as persisted in
/// `dave_settings.json`.
///
/// The key is stored as its [`egui::Key::name`] rather than as the enum:
/// egui's `Key` serde is feature-gated, and the name is stable across egui
/// bumps. Resolve it with [`LeaderKey::resolve`] once, not per frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LeaderKey {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// [`egui::Key::name`] of the key, e.g. `"Semicolon"`.
    pub key: String,
}

impl Default for LeaderKey {
    /// Ctrl+;: free in both Dave's keybindings and the chrome's, and
    /// vim-adjacent. Ctrl+Space is avoided for macOS input-source switching.
    fn default() -> Self {
        Self::from_press(egui::Modifiers::CTRL, egui::Key::Semicolon)
    }
}

impl LeaderKey {
    /// Record a key press as a leader key.
    pub fn from_press(modifiers: egui::Modifiers, key: egui::Key) -> Self {
        LeaderKey {
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            alt: modifiers.alt,
            key: key.name().to_owned(),
        }
    }

    /// Whether a press would make a usable leader. It needs Ctrl or Alt: a
    /// bare or Shift-only key is something you type.
    pub fn is_valid_press(modifiers: egui::Modifiers) -> bool {
        modifiers.ctrl || modifiers.alt
    }

    /// The modifiers to match exactly.
    pub fn modifiers(&self) -> egui::Modifiers {
        let mut modifiers = egui::Modifiers::NONE;
        if self.ctrl {
            modifiers = modifiers.plus(egui::Modifiers::CTRL);
        }
        if self.shift {
            modifiers = modifiers.plus(egui::Modifiers::SHIFT);
        }
        if self.alt {
            modifiers = modifiers.plus(egui::Modifiers::ALT);
        }
        modifiers
    }

    /// The stored key name as an [`egui::Key`], or `None` if it names no key
    /// (a hand-edited or future settings file).
    pub fn resolve(&self) -> Option<egui::Key> {
        egui::Key::from_name(&self.key)
    }
}

impl std::fmt::Display for LeaderKey {
    /// Human-readable form, e.g. `Ctrl+Shift+;`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        match self.resolve() {
            Some(key) => f.write_str(key.symbol_or_name()),
            None => f.write_str(&self.key),
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
            leader_key: LeaderKey::default(),
            session_env: BTreeMap::new(),
        }
    }

    /// Create settings from an existing ModelConfig (preserves env var values)
    pub fn from_model_config(config: &ModelConfig) -> Self {
        let provider = match config.backend {
            BackendType::OpenAI | BackendType::Remote => AiProvider::OpenAI,
            BackendType::Claude => AiProvider::Anthropic,
            BackendType::Codex => AiProvider::Codex,
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
            leader_key: LeaderKey::default(),
            session_env: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub struct ModelConfig {
    pub trial: bool,
    pub backend: BackendType,
    endpoint: Option<String>,
    model: String,
    api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
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

/// The environment [`ModelConfig::default`] reads, captured as data.
///
/// Lifting these out of the `Default` impl makes backend auto-detection a pure
/// function of its inputs — see [`ModelConfig::from_env`]. Testing it against
/// the real process environment isn't possible: a dev box that has selected a
/// backend (the documented way to choose one) or has `claude` on PATH takes a
/// different path than a bare CI runner, so a test written against
/// `Default::default()` silently asserts nothing on most machines.
#[derive(Debug, Default, Clone)]
pub struct EnvSnapshot {
    pub dave_api_key: Option<String>,
    pub openai_api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub claude_api_key: Option<String>,
    pub backend: Option<String>,
    pub model: Option<String>,
    pub endpoint: Option<String>,
}

impl EnvSnapshot {
    /// Read the variables from the process environment.
    pub fn from_process_env() -> Self {
        EnvSnapshot {
            dave_api_key: env::var("DAVE_API_KEY").ok(),
            openai_api_key: env::var("OPENAI_API_KEY").ok(),
            anthropic_api_key: env::var("ANTHROPIC_API_KEY").ok(),
            claude_api_key: env::var("CLAUDE_API_KEY").ok(),
            backend: env::var("DAVE_BACKEND").ok(),
            model: env::var("DAVE_MODEL").ok(),
            endpoint: env::var("DAVE_ENDPOINT").ok(),
        }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig::from_env(
            &EnvSnapshot::from_process_env(),
            has_binary_on_path("claude"),
            has_binary_on_path("codex"),
        )
    }
}

impl ModelConfig {
    /// Resolve a config from an environment snapshot and which agentic CLIs are
    /// installed.
    ///
    /// Backend precedence: an explicit `DAVE_BACKEND` wins; otherwise prefer an
    /// agentic backend whose CLI is on PATH (claude, then codex), then an
    /// Anthropic API key, then OpenAI with the built-in trial key. The PATH
    /// preference is what keeps Android — where neither CLI exists — off the
    /// agentic backends.
    pub fn from_env(env: &EnvSnapshot, has_claude: bool, has_codex: bool) -> Self {
        let api_key = env
            .dave_api_key
            .clone()
            .or_else(|| env.openai_api_key.clone());

        let anthropic_api_key = env
            .anthropic_api_key
            .clone()
            .or_else(|| env.claude_api_key.clone());

        // Determine backend: explicit env var takes precedence, otherwise auto-detect
        let backend = if let Some(backend_str) = env.backend.as_deref() {
            match backend_str.to_lowercase().as_str() {
                "claude" | "anthropic" => BackendType::Claude,
                "openai" => BackendType::OpenAI,
                "codex" => BackendType::Codex,
                _ => {
                    tracing::warn!(
                        "Unknown DAVE_BACKEND value: {}, defaulting to OpenAI",
                        backend_str
                    );
                    BackendType::OpenAI
                }
            }
        } else if has_claude {
            BackendType::Claude
        } else if has_codex {
            BackendType::Codex
        } else if anthropic_api_key.is_some() {
            BackendType::Claude
        } else {
            BackendType::OpenAI
        };

        // trial mode?
        let trial = api_key.is_none() && backend == BackendType::OpenAI;
        let api_key = if backend == BackendType::OpenAI {
            api_key.or(Some(DAVE_TRIAL.to_string()))
        } else {
            api_key
        };

        let model = env.model.clone().unwrap_or_else(|| match backend {
            BackendType::OpenAI => "gpt-4.1-mini".to_string(),
            BackendType::Claude => "claude-sonnet-4.5".to_string(),
            BackendType::Codex => AiProvider::Codex.default_model().to_string(),
            BackendType::Remote => String::new(),
        });

        ModelConfig {
            trial,
            backend,
            endpoint: env.endpoint.clone(),
            model,
            api_key,
            anthropic_api_key,
        }
    }

    pub fn ai_mode(&self) -> AiMode {
        match self.backend {
            BackendType::Claude | BackendType::Codex => AiMode::Agentic,
            BackendType::OpenAI | BackendType::Remote => AiMode::Chat,
        }
    }

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
            backend: BackendType::OpenAI, // Ollama uses OpenAI-compatible API
            endpoint: std::env::var("OLLAMA_HOST").ok().map(|h| h + "/v1"),
            model: "hhao/qwen2.5-coder-tools:latest".to_string(),
            api_key: None,
            anthropic_api_key: None,
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
            AiProvider::Codex => BackendType::Codex,
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

    /// Create a trial-mode config (uses embedded trial key with gpt-4.1-mini)
    pub fn trial() -> Self {
        ModelConfig {
            trial: true,
            backend: BackendType::OpenAI,
            endpoint: None,
            model: "gpt-4.1-mini".to_string(),
            api_key: Some(DAVE_TRIAL.to_string()),
            anthropic_api_key: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn env_with_backend(backend: &str) -> EnvSnapshot {
        EnvSnapshot {
            backend: Some(backend.to_string()),
            ..Default::default()
        }
    }

    /// `DAVE_BACKEND` overrides auto-detection, including when both CLIs are on
    /// PATH. An unrecognized value falls back to OpenAI rather than failing.
    #[test]
    fn explicit_backend_env_var_wins_over_path_detection() {
        let cases = [
            ("claude", BackendType::Claude),
            ("anthropic", BackendType::Claude),
            ("CLAUDE", BackendType::Claude),
            ("openai", BackendType::OpenAI),
            ("codex", BackendType::Codex),
            ("nonsense", BackendType::OpenAI),
        ];
        for (value, expected) in cases {
            let config = ModelConfig::from_env(&env_with_backend(value), true, true);
            assert_eq!(
                config.backend, expected,
                "DAVE_BACKEND={value:?} must select {expected:?} whatever is on PATH"
            );
        }
    }

    /// With no `DAVE_BACKEND`: claude on PATH, then codex on PATH, then an
    /// Anthropic key, then OpenAI. The PATH steps come first so a machine with
    /// neither CLI — Android — never lands on an agentic backend.
    #[test]
    fn backend_auto_detect_precedence() {
        let with_key = EnvSnapshot {
            anthropic_api_key: Some("sk-ant-x".to_string()),
            ..Default::default()
        };
        let bare = EnvSnapshot::default();

        assert_eq!(
            ModelConfig::from_env(&bare, true, true).backend,
            BackendType::Claude,
            "claude on PATH outranks codex"
        );
        assert_eq!(
            ModelConfig::from_env(&bare, false, true).backend,
            BackendType::Codex,
            "codex on PATH is next"
        );
        assert_eq!(
            ModelConfig::from_env(&with_key, false, false).backend,
            BackendType::Claude,
            "an Anthropic key selects Claude with no CLI installed"
        );
        assert_eq!(
            ModelConfig::from_env(&bare, false, false).backend,
            BackendType::OpenAI,
            "nothing installed and no keys falls back to OpenAI"
        );
        // The key must not outrank a CLI that is actually present.
        assert_eq!(
            ModelConfig::from_env(&with_key, false, true).backend,
            BackendType::Codex,
            "codex on PATH outranks an Anthropic key"
        );
    }

    /// Trial mode is OpenAI with no user-supplied key, and only then.
    #[test]
    fn trial_mode_only_when_openai_without_a_key() {
        let bare = ModelConfig::from_env(&EnvSnapshot::default(), false, false);
        assert!(bare.trial, "no keys, no CLIs: the OpenAI trial");
        assert_eq!(bare.api_key(), Some(DAVE_TRIAL));
        assert_eq!(bare.model(), "gpt-4.1-mini");

        let with_key = ModelConfig::from_env(
            &EnvSnapshot {
                openai_api_key: Some("sk-user".to_string()),
                ..Default::default()
            },
            false,
            false,
        );
        assert!(!with_key.trial, "a user key is not trial mode");
        assert_eq!(with_key.api_key(), Some("sk-user"));

        let claude = ModelConfig::from_env(&env_with_backend("claude"), false, false);
        assert!(!claude.trial, "trial mode is an OpenAI-only concept");
        assert_eq!(claude.api_key(), None, "no OpenAI trial key on Claude");
        assert_eq!(claude.model(), "claude-sonnet-4.5");
    }

    /// `DAVE_API_KEY` wins over `OPENAI_API_KEY`, and `ANTHROPIC_API_KEY` over
    /// `CLAUDE_API_KEY`.
    #[test]
    fn api_key_precedence() {
        let config = ModelConfig::from_env(
            &EnvSnapshot {
                dave_api_key: Some("sk-dave".to_string()),
                openai_api_key: Some("sk-openai".to_string()),
                anthropic_api_key: Some("sk-ant".to_string()),
                claude_api_key: Some("sk-claude".to_string()),
                ..Default::default()
            },
            false,
            false,
        );
        assert_eq!(config.api_key(), Some("sk-dave"));
        assert_eq!(config.anthropic_api_key.as_deref(), Some("sk-ant"));
    }

    /// The variables [`EnvSnapshot::from_process_env`] reads.
    const READ_VARS: [&str; 7] = [
        "DAVE_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CLAUDE_API_KEY",
        "DAVE_BACKEND",
        "DAVE_MODEL",
        "DAVE_ENDPOINT",
    ];

    /// Saves every variable in [`READ_VARS`] on construction and puts it back on
    /// drop, so a failing assertion can't leak a fixture value into the rest of
    /// the run.
    struct EnvGuard(Vec<(&'static str, Option<String>)>);

    impl EnvGuard {
        fn take() -> Self {
            EnvGuard(READ_VARS.iter().map(|k| (*k, env::var(k).ok())).collect())
        }

        /// Set `key`, or remove it when `value` is `None`.
        fn put(key: &str, value: Option<&str>) {
            match value {
                Some(v) => env::set_var(key, v),
                None => env::remove_var(key),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                EnvGuard::put(key, value.as_deref());
            }
        }
    }

    /// Each documented variable lands in its own [`EnvSnapshot`] field.
    ///
    /// The tests above pin [`ModelConfig::from_env`], which is a pure function of
    /// the snapshot — but the mapping from variable *name* to field lives only in
    /// [`EnvSnapshot::from_process_env`], whose sole caller is [`Default`], whose
    /// sole caller is production. So a transposed or misspelled name is invisible
    /// to every other test in the crate; only going through the process
    /// environment can see it. Each value names its own variable, so a
    /// transposition fails rather than reading through.
    #[test]
    #[serial]
    fn from_process_env_maps_each_documented_variable() {
        let _guard = EnvGuard::take();
        for key in READ_VARS {
            EnvGuard::put(key, Some(&format!("v-{key}")));
        }

        let snap = EnvSnapshot::from_process_env();
        assert_eq!(snap.dave_api_key.as_deref(), Some("v-DAVE_API_KEY"));
        assert_eq!(snap.openai_api_key.as_deref(), Some("v-OPENAI_API_KEY"));
        assert_eq!(
            snap.anthropic_api_key.as_deref(),
            Some("v-ANTHROPIC_API_KEY")
        );
        assert_eq!(snap.claude_api_key.as_deref(), Some("v-CLAUDE_API_KEY"));
        assert_eq!(snap.backend.as_deref(), Some("v-DAVE_BACKEND"));
        assert_eq!(snap.model.as_deref(), Some("v-DAVE_MODEL"));
        assert_eq!(snap.endpoint.as_deref(), Some("v-DAVE_ENDPOINT"));
    }

    /// An unset variable reads as `None` rather than an empty string, which is
    /// what makes the `or_else` key fallbacks and the `DAVE_BACKEND` auto-detect
    /// branch fire.
    #[test]
    #[serial]
    fn from_process_env_reports_unset_variables_as_none() {
        let _guard = EnvGuard::take();
        for key in READ_VARS {
            EnvGuard::put(key, None);
        }

        let snap = EnvSnapshot::from_process_env();
        assert_eq!(snap.dave_api_key, None);
        assert_eq!(snap.openai_api_key, None);
        assert_eq!(snap.anthropic_api_key, None);
        assert_eq!(snap.claude_api_key, None);
        assert_eq!(snap.backend, None);
        assert_eq!(snap.model, None);
        assert_eq!(snap.endpoint, None);
    }

    /// `DAVE_MODEL` and `DAVE_ENDPOINT` are passed through as given.
    #[test]
    fn model_and_endpoint_come_from_the_environment() {
        let config = ModelConfig::from_env(
            &EnvSnapshot {
                model: Some("my-model".to_string()),
                endpoint: Some("http://localhost:1234/v1".to_string()),
                ..Default::default()
            },
            true,
            false,
        );
        assert_eq!(config.model(), "my-model");
        assert_eq!(config.endpoint(), Some("http://localhost:1234/v1"));
    }

    /// A `dave_settings.json` written before the leader key existed still
    /// loads, with the default leader.
    #[test]
    fn settings_without_a_leader_key_still_load() {
        let json = r#"{"provider":"OpenAI","model":"gpt-4o","endpoint":null,"api_key":null}"#;
        let settings: DaveSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.leader_key, LeaderKey::default());
        assert_eq!(settings.leader_key.to_string(), "Ctrl+;");
    }

    /// A `dave_settings.json` written before `session_env` existed still loads,
    /// with no extra session env, and one that sets it round-trips.
    #[test]
    fn settings_without_session_env_still_load() {
        let json = r#"{"provider":"OpenAI","model":"gpt-4o","endpoint":null,"api_key":null}"#;
        let settings: DaveSettings = serde_json::from_str(json).unwrap();
        assert!(settings.session_env.is_empty());

        let json = r#"{"provider":"OpenAI","model":"gpt-4o","endpoint":null,"api_key":null,
            "session_env":{"HEADWAY_COMMENT_NSEC_FILE":"/keys/agent"}}"#;
        let settings: DaveSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            settings
                .session_env
                .get("HEADWAY_COMMENT_NSEC_FILE")
                .map(String::as_str),
            Some("/keys/agent")
        );
    }
}
