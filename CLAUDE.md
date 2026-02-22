# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Sheesh-RS

A TUI app for managing SSH connections with an embedded LLM assistant.

# Commands

```bash
cargo build                   # debug build
cargo build --release         # release build
cargo run                     # run the app
cargo clippy                  # lint
cargo test                    # run tests (none yet, but for future use)
```

Logs are written to `./logs` (relative to the working directory where the binary is run).

# Main Features
- CRUD SSH connections stored in `~/.ssh/config` (comments above `Host` blocks = description)
- Connect to SSH sessions via an embedded PTY (`portable-pty` spawns `ssh`)
- Listing view: 65/35 split — connection list (left) + detail panel (right)
- Connected view: 60/40 split — terminal (left) + LLM chat (right)
- On connect: switch to connected view; SSH errors appear in the PTY, not as popups
- On SSH process exit: terminal shows `○ disconnected`, user presses `ctrl+d` to go back
- Only PTY-level OS failures show a popup

# Architecture

## File Structure
```
src/
├── main.rs           — Sheesh struct, event loop, layout, state transitions
├── app.rs            — AppState enum (Listing / Connected), ConnectedFocus
├── ssh.rs            — SSHConnection model, ssh_args() builder
├── config.rs         — ~/.ssh/config parser + writer
├── event.rs          — Action enum, crossterm key mapper
├── tabs/
│   ├── mod.rs        — Tab trait (render, handle_event, title, key_hints)
│   ├── listing.rs    — connection list + detail panel, add/edit/delete/filter
│   ├── terminal.rs   — embedded PTY, output capture, keystroke passthrough
│   └── llm.rs        — LLM chat panel, context injection, async via mpsc channel
├── llm/
│   ├── mod.rs        — LLMProvider trait, Message, LLMConfig, spawn_completion()
│   ├── anthropic.rs  — Anthropic API (reqwest blocking)
│   ├── openai.rs     — OpenAI API
│   └── ollama.rs     — Ollama local API
└── ui/
    ├── theme.rs      — color palette (Theme struct)
    └── keybindings.rs — bottom bar renderer (render_keybindings)
```

## Key Design Decisions
- `Tab` trait: every panel implements `render`, `handle_event`, `title`, `key_hints`
- LLM calls run in background threads via `spawn_completion()`, results sent back via `mpsc::channel` (no async runtime)
- SSH connections parsed from / written to `~/.ssh/config`; description = `# comment` above `Host` block
- `TerminalTab` captures PTY output into an `Arc<Mutex<Vec<String>>>` line buffer
- `c` in terminal focus sends last 50 lines as context to the LLM panel
- Provider is selected via `~/.config/sheesh/config.toml` (`[llm] provider = "anthropic"|"openai"|"ollama"`)
- Mouse support: left-click focuses the panel that was clicked; terminal also receives the click for text selection
- `app.rs` contains a legacy `App` struct (marked `#[allow(dead_code)]`); actual app state lives in `Sheesh` in `main.rs`

## LLM Configuration (`~/.config/sheesh/config.toml`)
```toml
[llm]
provider = "anthropic"       # "anthropic" | "openai" | "ollama"
model = "claude-sonnet-4-6"  # for anthropic/openai
api_key_env = "ANTHROPIC_API_KEY"
ollama_host = "http://localhost:11434"
ollama_model = "llama3"
```
API keys are read from environment variables (not stored in the config file).

# Keybindings
| Key | Context | Action |
|-----|---------|--------|
| `j/k` | Listing | Navigate connections |
| `enter` | Listing | Connect |
| `a/e/d` | Listing | Add / Edit / Delete |
| `/` | Listing | Filter |
| `F2` | Connected | Cycle focus (terminal ↔ LLM) |
| `F3` | Connected | Send last 50 terminal lines to LLM |
| `c` | Terminal focused | Send last 50 lines to LLM |
| `ctrl+d` | Terminal focused | Disconnect |
| `enter` | LLM focused | Send message |
| `q` | Anywhere | Quit |