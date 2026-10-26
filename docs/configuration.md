# Configuration Reference

## Environment Variables

| Variable | Default | Required | Description |
|---|---|---|---|
| `ZERO_G_BIND` | `127.0.0.1:8080` | No | Listening `host:port` bind address |
| `ZERO_G_ZEN_URL` | `http://127.0.0.1:8090/v1` | No | Base URL of the OpenCode Zen shim. On startup zero-g probes this URL and auto-spawns a sibling `zen-shim` binary when nothing answers; set it explicitly when the shim runs elsewhere (e.g. `http://zen-shim:8090/v1` in compose) |
| `ZERO_G_ZEN_API_KEY` | `public` | No | Bearer token for OpenCode Zen upstream (`public` for keyless free tier, or a Zen API key) |
| `ZERO_G_OAUTH_CLIENT_ID` | None (reads `.env`) | No | Google OAuth desktop client ID for sign-in/refresh |
| `ZERO_G_OAUTH_CLIENT_SECRET` | None (reads `.env`) | No | Google OAuth desktop client secret |
| `ZERO_G_API_TOKEN_AGY` | None | No | Optional bearer token protecting `/agy/v1/*` routes |
| `ZERO_G_API_TOKEN_CODEX` | None | No | Optional bearer token protecting `/codex/v1/*` routes |
| `ZERO_G_API_TOKEN_ZEN` | None | No | Optional bearer token protecting `/zen/v1/*` routes |
## Obtaining OAuth Credentials

`zero-g login` requires desktop OAuth client credentials to authenticate with Google:

1. Navigate to [Google Cloud Console](https://console.cloud.google.com/) $\to$ **APIs & Services** $\to$ **Credentials**.
2. Click **Create Credentials** $\to$ **OAuth client ID** $\to$ select Application type: **Desktop app**.
3. Add the required scopes (`cloud-platform`, `userinfo.email`, `userinfo.profile`, `cclog`, `experimentsandconfigs`).
4. Copy the Client ID and Client Secret into `.env` as `ZERO_G_OAUTH_CLIENT_ID` and `ZERO_G_OAUTH_CLIENT_SECRET`.
## CLI Commands and Flags

### `zero-g serve`

Run the OpenAI-compatible HTTP proxy. All runtime options (bind address, Zen shim URL/key) are configured via environment variables (`ZERO_G_BIND`, `ZERO_G_ZEN_URL`, `ZERO_G_ZEN_API_KEY`).
### `zero-g login`

Start a temporary OAuth callback listener on `127.0.0.1:51121`, open the browser consent flow, exchange tokens, and discover the Cloud Code Assist project.

### `zero-g models`

Query the active Antigravity subscription and display all available wire model IDs, token limits, and reasoning capabilities.

### `zero-g status`

Print the active GCP companion project ID, account email, token expiry, and refresh token presence.

## Credentials Storage

`zero-g` looks for credentials in the following order:

### Google Antigravity

1. Environment override: `ZERO_G_CREDENTIALS_AGY` or `ZERO_G_CREDENTIALS`.
2. Local file: `credentials-agy.json` in the current working directory.
3. User file: `$XDG_CONFIG_HOME/zero-g/credentials-agy.json` (falling back to `~/.config/zero-g/credentials-agy.json`).

#### Antigravity Schema (`credentials-agy.json`)

```json
{
  "access_token": "ya29...",
  "refresh_token": "1//0...",
  "project_id": "aicode-consumers",
  "expires_at": 1792820000000,
  "email": "user@example.com"
}
```

### OpenAI Codex (ChatGPT OAuth)

1. Environment override: `ZERO_G_CREDENTIALS_CODEX` (or token via `OPENAI_CODEX_OAUTH_TOKEN`).
2. Local file: `credentials-codex.json` in the current working directory.
3. User file: `$XDG_CONFIG_HOME/zero-g/credentials-codex.json` (falling back to `~/.config/zero-g/credentials-codex.json`).

#### Codex Schema (`credentials-codex.json`)

```json
{
  "access_token": "eyJhbGci...",
  "refresh_token": "rt_...",
  "account_id": "acc_123456",
  "email": "user@example.com",
  "plan_type": "pro",
  "residency": "us",
  "expires_at": 1792820000000
}
```
