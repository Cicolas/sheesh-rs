<p align="center">
  <img src="logo.png" alt="sheesh-rs" width="400"/>
</p>

A terminal UI for managing SSH connections with an embedded LLM assistant.

![demo](demo.jpg)

## Features

- **Connection manager** — CRUD SSH connections backed by `~/.ssh/config`; comments above a `Host` block become its description
- **Embedded terminal** — connects over a PTY so the full SSH session runs inside the TUI
- **LLM sidebar** — chat with an AI assistant while connected; press `F3` to send the last 50 lines of terminal output as context
- **Multi-provider LLM** — Anthropic (default), OpenAI, or a local Ollama instance

## Installation

```bash
cargo build --release
# binary at target/release/sheesh-rs
```

## Configuration

Create `~/.config/sheesh/config.toml`:

```toml
[llm]
provider = "anthropic"        # "anthropic" | "openai" | "ollama"
model = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_API_KEY"

# Ollama only
ollama_host = "http://localhost:11434"
ollama_model = "llama3"
```

API keys are read from the environment variable named by `api_key_env`.

## Keybindings

| Key | Context | Action |
|-----|---------|--------|
| `j / k` | Listing | Navigate |
| `enter` | Listing | Connect |
| `a / e / d` | Listing | Add / Edit / Delete |
| `/` | Listing | Filter |
| `F2` | Connected | Switch panel (terminal ↔ LLM) |
| `F3` | Connected | Send terminal context to LLM |
| `ctrl+d` | Terminal | Disconnect |
| `enter` | LLM | Send message |
| `q` | Anywhere | Quit |

## License

See [LICENSE](LICENSE).
