<div align="center">

# zero-g

**OpenAI-compatible proxy for Google Antigravity and OpenAI Codex subscriptions.**

[![CI](https://img.shields.io/github/actions/workflow/status/Delnegend/zero-g/ci.yml?branch=main&style=flat-square)](https://github.com/Delnegend/zero-g/actions)
[![Release](https://img.shields.io/github/v/release/Delnegend/zero-g?style=flat-square)](https://github.com/Delnegend/zero-g/releases)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat-square)](#license)

</div>

---

## Quick Start

Get running in less than 60 seconds:

```bash
# 1. Install (Linux and macOS)
brew install Delnegend/tap/zero-g

# 2. Run
zero-g serve

# 3. Verify
curl http://localhost:8080/agy/v1/models

Source builds from checkout: `cargo install --path .` (last resort only).

## Highlights

- **Standard OpenAI API per Provider** — Explicit `/agy/v1`, `/codex/v1`, and `/zen/v1` routes supporting `chat/completions`, `responses`, and `models`.
- **Multi-Provider Backends** — Seamless proxying for Google Antigravity (`/agy`), OpenAI Codex (`/codex`), and OpenCode Zen (`/zen`).
- **Auto-Refresh** — Refreshes expired OAuth tokens in the background automatically.
- **Claude & Gemini Support** — Formats Anthropic thinking beta headers, system instructions, and tool calling signatures automatically.
- **Multi-Turn Tool Calling** — End-to-end function declaration, execution, and tool response turn merging.
## Common Commands

```bash
# Start proxy (configured via .env or defaults to 127.0.0.1:8080)
zero-g serve
```

| Command | Description |
|---|---|
| `serve` | Start HTTP proxy server (default) |
| `login <provider>` | Interactive OAuth sign-in flow (`zero-g login agy` or `zero-g login codex`) |
| `models <provider>` | List available models (`zero-g models agy`, `zero-g models codex`, `zero-g models zen`) |
| `status [provider]` | Print token validity, project ID, and account status |

For all environment variables, see **[Configuration Reference](docs/configuration.md)**.

## Architecture

```mermaid
flowchart LR
    Client[OpenAI Client] -->|/agy/v1, /codex/v1, /zen/v1| ZeroG[zero-g Proxy]
    ZeroG -->|OAuth + SSE| CloudCode[Google Cloud Code Assist]
```

For component boundaries, internal translation, and state invariants, see **[Architecture Guide](docs/architecture.md)**.

## Documentation

- **[Configuration](docs/configuration.md)** — Environment variables, flags, and credentials schema.
- **[Architecture](docs/architecture.md)** — Request envelopes, streaming translation, and invariants.
- **[Gateway Integration](docs/gateways.md)** — LiteLLM and Bifrost provider configuration.
- **[Deployment](docs/deployment.md)** — Homebrew services, systemd units, and upgrades.
- **[Development](docs/development.md)** — Prerequisites, `just check` gate, and test suite.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
