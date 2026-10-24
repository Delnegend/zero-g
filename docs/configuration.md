# Configuration Reference

## Environment Variables

| Variable | Default | Required | Description |
|---|---|---|---|
| `ZERO_G_HOST` | `127.0.0.1` | No | Listening network interface or IP address |
| `ZERO_G_PORT` | `8080` | No | Listening TCP port |
| `ZERO_G_BIND` | None | No | Combined `host:port` bind address override |
| `ZERO_G_API_KEY` | None | No | Bearer token required for proxy client requests |
| `OAUTH_CLIENT_ID` | None (reads `.env`) | No | Google OAuth desktop client ID for sign-in/refresh |
| `OAUTH_CLIENT_SECRET` | None (reads `.env`) | No | Google OAuth desktop client secret |
| `ANTIGRAVITY_ACCESS_TOKEN` | None | No | Explicit Google OAuth access token override |
| `ANTIGRAVITY_REFRESH_TOKEN` | None | No | Explicit Google OAuth refresh token override |
| `ANTIGRAVITY_PROJECT_ID` | None | No | Google Cloud Code Assist companion project ID |

## Obtaining OAuth Credentials

`zero-g login` requires desktop OAuth client credentials to authenticate with Google:

1. Navigate to [Google Cloud Console](https://console.cloud.google.com/) $\to$ **APIs & Services** $\to$ **Credentials**.
2. Click **Create Credentials** $\to$ **OAuth client ID** $\to$ select Application type: **Desktop app**.
3. Add the required scopes (`cloud-platform`, `userinfo.email`, `userinfo.profile`, `cclog`, `experimentsandconfigs`).
4. Copy the Client ID and Client Secret into `.env` as `OAUTH_CLIENT_ID` and `OAUTH_CLIENT_SECRET`.
## CLI Commands and Flags

### `zero-g serve`

Run the OpenAI-compatible HTTP proxy. All runtime options (listening host, port, API key authentication) are configured exclusively via environment variables (`ZERO_G_HOST`, `ZERO_G_PORT`, `ZERO_G_BIND`, `ZERO_G_API_KEY`).
### `zero-g login`

Start a temporary OAuth callback listener on `127.0.0.1:51121`, open the browser consent flow, exchange tokens, and discover the Cloud Code Assist project.

### `zero-g models`

Query the active Antigravity subscription and display all available wire model IDs, token limits, and reasoning capabilities.

### `zero-g status`

Print the active GCP companion project ID, account email, token expiry, and refresh token presence.

## Credentials Storage

`zero-g` looks for credentials in the following order:

1. Local configuration file: `credentials.json` in the current working directory.
2. User configuration file: `~/.config/zero-g/credentials.json`.
3. Environment variables: `ANTIGRAVITY_ACCESS_TOKEN` and `ANTIGRAVITY_PROJECT_ID`.

### Credentials JSON Schema

```json
{
  "access_token": "ya29...",
  "refresh_token": "1//0...",
  "project_id": "aicode-consumers",
  "expires_at": 1792820000000,
  "email": "user@example.com"
}
```

## Model Aliases

The proxy maps standard client model names to Antigravity wire routes:

| Client Model Name | Upstream Wire Model | Family |
|---|---|---|
| `claude-3-5-sonnet`, `claude-sonnet` | `claude-sonnet-5-5-medium` | Anthropic Claude |
| `claude-opus` | `claude-opus-5-5-medium` | Anthropic Claude |
| `gemini-flash` | `gemini-3.8-flash-low` | Google Gemini |
| `gemini-pro`, `gemini-4-argon` | `gemini-4-argon-low` | Google Gemini |
| `gpt-oss` | `gpt-oss-120b-medium` | OpenAI GPT-OSS |
