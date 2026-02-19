//! Integration tests for OpenAI backend / trial key
//!
//! These tests verify that the OpenAI backend can connect and stream
//! responses using the embedded trial API key.
//!
//! Run with: cargo test -p notedeck_dave --test openai_integration -- --ignored

use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequest,
};
use async_openai::Client;
use futures::StreamExt;
use notedeck_dave::config::ModelConfig;

/// Test that the trial key can authenticate and get a streamed response.
#[tokio::test]
#[ignore = "Requires network access to OpenAI API"]
async fn test_trial_key_streams_response() {
    let config = ModelConfig::trial();
    let client = Client::with_config(config.to_api());

    let message = ChatCompletionRequestUserMessageArgs::default()
        .content("Say hello in one word.")
        .build()
        .expect("build user message");

    let request = CreateChatCompletionRequest {
        model: config.model().to_string(),
        stream: Some(true),
        messages: vec![ChatCompletionRequestMessage::User(message)],
        ..Default::default()
    };

    let mut stream = client
        .chat()
        .create_stream(request)
        .await
        .expect("Failed to create stream - trial key may be invalid or expired");

    let mut received_text = String::new();
    let mut chunk_count = 0;

    while let Some(result) = stream.next().await {
        let response = result.expect("Stream chunk error");
        for choice in &response.choices {
            if let Some(content) = &choice.delta.content {
                received_text.push_str(content);
                chunk_count += 1;
            }
        }
    }

    assert!(
        !received_text.is_empty(),
        "Should receive text from OpenAI. Got empty response."
    );
    println!(
        "Trial key works: received {} chunks, text: {:?}",
        chunk_count, received_text
    );
}

/// Test that a non-streaming request also works with the trial key.
#[tokio::test]
#[ignore = "Requires network access to OpenAI API"]
async fn test_trial_key_non_streaming() {
    let config = ModelConfig::trial();
    let client = Client::with_config(config.to_api());

    let message = ChatCompletionRequestUserMessageArgs::default()
        .content("Reply with exactly: OK")
        .build()
        .expect("build user message");

    let request = CreateChatCompletionRequest {
        model: config.model().to_string(),
        stream: Some(false),
        messages: vec![ChatCompletionRequestMessage::User(message)],
        ..Default::default()
    };

    let response = client
        .chat()
        .create(request)
        .await
        .expect("Failed to create completion - trial key may be invalid or expired");

    let text = response.choices[0].message.content.as_deref().unwrap_or("");

    assert!(
        !text.is_empty(),
        "Should receive non-empty response from OpenAI"
    );
    println!("Non-streaming response: {:?}", text);
}

/// Diagnostic: check which models the trial key project has access to.
#[tokio::test]
#[ignore = "Requires network access to OpenAI API"]
async fn test_trial_key_model_access() {
    let config = ModelConfig::trial();
    let client = Client::with_config(config.to_api());

    let models_to_try = ["gpt-5.2", "gpt-4.1-mini", "gpt-4.1-nano", "gpt-4.1"];

    for model in models_to_try {
        let message = ChatCompletionRequestUserMessageArgs::default()
            .content("Say hi")
            .build()
            .expect("build user message");

        let request = CreateChatCompletionRequest {
            model: model.to_string(),
            stream: Some(false),
            messages: vec![ChatCompletionRequestMessage::User(message)],
            max_tokens: Some(5),
            ..Default::default()
        };

        match client.chat().create(request).await {
            Ok(_) => println!("  OK: {}", model),
            Err(e) => println!("FAIL: {} - {}", model, e),
        }
    }
}

/// Test that ModelConfig::trial() produces the expected configuration.
#[test]
fn test_trial_config_values() {
    let config = ModelConfig::trial();

    assert!(config.trial, "trial flag should be true");
    assert_eq!(config.model(), "gpt-4.1-mini");
    assert!(
        config.api_key().is_some(),
        "Trial config should have an API key"
    );
    assert!(
        config.api_key().unwrap().starts_with("sk-"),
        "Trial API key should start with sk-"
    );
    assert!(
        config.endpoint().is_none(),
        "Trial config should use default OpenAI endpoint"
    );
}

/// Test that ModelConfig::default() falls back to trial key when no env vars are set.
/// This verifies the Android fix (no longer defaults to Remote backend).
#[test]
fn test_default_config_uses_openai_without_env_vars() {
    // Note: This test's behavior depends on environment variables.
    // When DAVE_API_KEY, OPENAI_API_KEY, ANTHROPIC_API_KEY, and CLAUDE_API_KEY
    // are all unset, it should default to OpenAI with trial key.
    let config = ModelConfig::default();

    // If no API keys are set in the environment, we should get OpenAI (not Remote)
    if std::env::var("DAVE_API_KEY").is_err()
        && std::env::var("OPENAI_API_KEY").is_err()
        && std::env::var("ANTHROPIC_API_KEY").is_err()
        && std::env::var("CLAUDE_API_KEY").is_err()
        && std::env::var("DAVE_BACKEND").is_err()
    {
        assert!(
            config.trial,
            "Should be in trial mode when no API keys are set"
        );
        assert!(
            config.api_key().is_some(),
            "Should have trial API key when no env vars are set"
        );
        assert_eq!(config.model(), "gpt-4.1-mini");
    }
}
