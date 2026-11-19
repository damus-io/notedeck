# OpenAI client config never reads the process env

Card: headway:notedeck/project-idea-action
OpenAIConfig::new() reads OPENAI_API_KEY: a custom endpoint gets the user's key.
openai_config blanks it for spec providers; ModelConfig::to_api still leaks it.

- headway:dave/pumpkin-method-message — found it only by reading async-openai's `Default`; nothing at the call site hints the env is read
