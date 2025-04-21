# Dave - The Nostr AI Assistant

Dave is an AI-powered assistant for the Nostr protocol, built as a Notedeck application. It provides a conversational interface that can search, analyze, and present Nostr notes to users.

## Overview

Dave demonstrates how to build a feature-rich application on the Notedeck platform that interacts with Nostr content. It serves both as a useful tool for Nostr users and as a reference implementation for developers building Notedeck apps.

## Features

- [x] Interactive 3D avatar with WebGPU rendering
- [x] Natural language conversations with AI
- [x] Query and search the local Nostr database for notes
- [x] Present and render notes to the user
- [x] Tool-based architecture for AI actions
- [ ] Context-aware searching (home, profile, or global scope)
- [ ] Chat history persistence
- [ ] Anonymous [lmzap](https://jb55.com/lmzap) backend

## Technical Details

Dave uses:

- Egui for UI rendering
- WebGPU for 3D avatar visualization
- OpenAI API (or Ollama with compatible models)
- NostrDB for efficient note storage and querying
- Async Rust for non-blocking API interactions

## Architecture

Dave is structured around several key components:

1. **UI Layer** - Handles rendering and user interactions
2. **Avatar** - 3D representation with WebGPU rendering
3. **AI Client** - Connects to language models via OpenAI or Ollama
4. **Tools System** - Provides structured ways for the AI to interact with Nostr data
5. **Message Handler** - Manages conversation state and message processing

## Usage as a Reference

Dave serves as an excellent reference for developers looking to:

- Build conversational interfaces in Notedeck
- Implement 3D rendering with WebGPU in Rust applications
- Create tool-based AI agents that can take actions in response to user requests
- Query and present Nostr content in custom applications

## Getting Started

1. Clone the repository
2. Set up your API keys for OpenAI or configure Ollama
   ```
   export OPENAI_API_KEY=your_api_key_here
   # or for Ollama
   export OLLAMA_HOST=http://localhost:11434
   ```
3. Build and run the Notedeck application with Dave

## Configuration

Dave can be configured to use different AI backends:

- OpenAI API (default) - Set the `OPENAI_API_KEY` environment variable
- Ollama - Use a compatible model like `hhao/qwen2.5-coder-tools` and set the `OLLAMA_HOST` environment variable

## Contributing

Contributions are welcome! See the issues list for planned features and improvements.

## License

GPL

## Related Projects

- [nostrdb](https://github.com/damus-io/nostrdb) - Embedded database for Nostr notes
