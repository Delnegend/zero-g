# Configuration Reference

## Environment Variables

| Variable | Default | Required | Description |
|---|---|---|---|
| `ZERO_G_BIND` | `127.0.0.1:8080` | No | Listening `host:port` bind address |
| `ZERO_G_ZEN_URL` | `http://127.0.0.1:8090/v1` | No | Base URL of the OpenCode Zen Bun shim (TLS proxy) |
| `ZERO_G_OAUTH_CLIENT_ID` | None (reads `.env`) | No | Google OAuth desktop client ID for sign-in/refresh |
| `ZERO_G_OAUTH_CLIENT_SECRET` | None (reads `.env`) | No | Google OAuth desktop client secret |

## Obtaining OAuth Credentials

`zero-g login` requires desktop OAuth client credentials to authenticate with Google:

1. Navigate to [Google Cloud Console](https://console.cloud.google.com/) $\to$ **APIs & Services** $\to$ **Credentials**.
2. Click **Create Credentials** $\to$ **OAuth client ID** $\to$ select Application type: **Desktop app**.
3. Add the required scopes (`cloud-platform`, `userinfo.email`, `userinfo.profile`, `cclog`, `experimentsandconfigs`).
4. Copy the Client ID and Client Secret into `.env` as `ZERO_G_OAUTH_CLIENT_ID` and `ZERO_G_OAUTH_CLIENT_SECRET`.
## CLI Commands and Flags

### `zero-g serve`

Run the OpenAI-compatible HTTP proxy. All runtime options (bind address, API key authentication) are configured via environment variables (`ZERO_G_BIND`, `ZERO_G_API_KEY`).
### `zero-g login`

Start a temporary OAuth callback listener on `127.0.0.1:51121`, open the browser consent flow, exchange tokens, and discover the Cloud Code Assist project.

### `zero-g models`

Query the active Antigravity subscription and display all available wire model IDs, token limits, and reasoning capabilities.

### `zero-g status`

Print the active GCP companion project ID, account email, token expiry, and refresh token presence.

## Credentials Storage

`zero-g` looks for credentials in the following order:

1. Local configuration file: `credentials.json` in the current working directory.
2. User configuration file: `$XDG_CONFIG_HOME/zero-g/credentials.json` (falling back to `~/.config/zero-g/credentials.json`).

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
