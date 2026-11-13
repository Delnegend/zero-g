<div align="center">

# zero-g

**OpenAI-compatible AI gateway for multiple providers.**

[![CI](https://img.shields.io/github/actions/workflow/status/Delnegend/zero-g/ci.yml?branch=main&style=flat-square)](https://github.com/Delnegend/zero-g/actions)
[![Release](https://img.shields.io/github/v/release/Delnegend/zero-g?style=flat-square)](https://github.com/Delnegend/zero-g/releases)
[![License](https://img.shields.io/badge/license-MIT%20%7C%20Apache--2.0-blue?style=flat-square)](LICENSE-MIT)

</div>

---

## Quick Start

Download the Compose file and example settings, then set a stable encryption key
and a strong dashboard password in `.env`:

```bash
mkdir zero-g && cd zero-g
curl -fsSLo compose.yml https://raw.githubusercontent.com/Delnegend/zero-g/main/compose.yml
curl -fsSLo .env https://raw.githubusercontent.com/Delnegend/zero-g/main/.env.example
chmod 600 .env
# Generate the encryption key with: openssl rand -base64 32
# Edit .env to set ZERO_G_ENCRYPTION_KEY and ZERO_G_DASHBOARD_PASSWORD.
docker compose pull && docker compose up -d
curl --fail http://127.0.0.1:8080/health
```

Open `http://127.0.0.1:8080/` and sign in. Create the Antigravity provider under **Providers**, add a credential, and sign in. Under **Virtual keys**, create a key and copy it immediately; it is shown only once. Set `ZERO_G_VIRTUAL_KEY` to the copied value and use it as the bearer token for every inference request, including model discovery:

```bash
curl -H "Authorization: Bearer $ZERO_G_VIRTUAL_KEY" http://127.0.0.1:8080/v1/models
```

## Highlights

- **Unified API**: All providers share `/v1`; model IDs include the provider ID. OpenRouter Decisions models use `/v1/decisions`.
- **Multi-Provider Backends**: Connect Antigravity, Codex, Claude Code, Cursor, Grok, Grok Build, OpenCode Zen and Go, OpenRouter, Command Code, ChatJimmy, and OpenAI-compatible upstreams.
- **OAuth Refresh**: Refreshes supported access tokens on demand when they approach expiration.
- **Claude and Gemini Support**: Formats Anthropic thinking beta headers, system instructions, and tool calling signatures automatically.
- **Multi-Turn Tool Calling**: End-to-end function declaration, execution, and tool response turn merging.

## Documentation

- **[Architecture](docs/architecture.md)**: Request envelopes, streaming translation, and system invariants.
- **[Development](docs/development.md)**: Prerequisites, `just check` gate, and test suite.
- **[Deployment](docs/deployment.md)**: Container setup, environment configuration, updates, and backups.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
