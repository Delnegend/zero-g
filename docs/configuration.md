# Configuration Reference

## Bootstrap environment

All commands require `ZERO_G_ENCRYPTION_KEY`: standard Base64 encoding of exactly 32 random bytes. Generate it once with `openssl rand -base64 32`, keep it stable, and back it up securely. Changing or losing it makes encrypted secrets unreadable. The application also reads a `.env` file in its working directory; protect that file.

| Variable | Default | Purpose |
|---|---|---|
| `ZERO_G_ENCRYPTION_KEY` | None | Required encryption key for stored secrets |
| `ZERO_G_LOG_DB` | `zero-g-logs.db` | Local Turso database path; container default `/app/data/zero-g-logs.db` |
| `ZERO_G_DASHBOARD_AUTH` | None | Required for `serve`: `basic`, `oidc`, or explicit `disabled` |
| `ZERO_G_DASHBOARD_ORIGIN` | None | Required for `serve` in every mode: exact dashboard origin, HTTPS except loopback HTTP locally; protects dashboard writes |
| `ZERO_G_DASHBOARD_USERNAME`, `ZERO_G_DASHBOARD_PASSWORD` | None | Required in Basic mode; username cannot contain `:` |
| `ZERO_G_OIDC_ISSUER`, `ZERO_G_OIDC_CLIENT_ID`, `ZERO_G_OIDC_CLIENT_SECRET` | None | Required in OIDC mode |
| `ZERO_G_OIDC_ALLOWED_EMAILS` | None | OIDC comma-separated allowlist of verified email addresses |
| `ZERO_G_OIDC_REDIRECT_URL` | None | OIDC callback URL ending in `/auth/callback`: HTTPS except loopback HTTP for local development |

Register the exact redirect URL with the identity provider. OIDC sessions expire after eight hours and are lost on restart. Never disable dashboard authentication on an untrusted network; use HTTPS outside localhost.

## Dashboard settings and virtual keys

Sign in to the dashboard and open **Settings** to configure the persisted listening address (default `127.0.0.1:8080`), log retention (default 30 days, range 1–36500), Zen upstream URL and API key, and Google OAuth desktop client ID and secret. Bind changes require a restart. Secrets are encrypted at rest; blank secret fields retain the existing value unless explicitly cleared. The Zen URL may be set to `http://zen-shim:8090/v1` for the supplied Compose setup.

Create a key under **Virtual keys**, copy it when displayed (it cannot be retrieved later), and supply `Authorization: Bearer <key>` to all provider inference and model endpoints. Choose provider and model restrictions as needed; selecting no providers or models permits all. Revoke compromised keys in the same screen. Dashboard authentication and virtual keys are separate controls.

## Google OAuth

Create a Desktop app OAuth client in [Google Cloud Console](https://console.cloud.google.com/) under **APIs & Services → Credentials**, with the required `cloud-platform`, `userinfo.email`, `userinfo.profile`, `cclog` and `experimentsandconfigs` scopes. Enter its client ID and secret in Settings. Legacy `ZERO_G_OAUTH_CLIENT_ID` and `ZERO_G_OAUTH_CLIENT_SECRET` are imported once on first startup when corresponding settings are absent; subsequent changes belong in the dashboard.

Open **Providers**, start Google sign-in, and follow the authorization link. When your browser is on the server machine, the local callback can complete sign-in automatically. If the browser is on another machine, copy the entire redirected loopback URL from its address bar and paste it into the dashboard, even if the loopback page does not load. The URL contains a temporary authorization code; keep it private. For Codex, start sign-in under **Providers**, open the device authorization link, and enter the displayed code; the dashboard waits for completion.

## Commands and credentials

`zero-g serve` starts the server. As an alternative to dashboard sign-in, `zero-g login agy` and `zero-g login codex` perform provider sign-in (Codex also supports `--device`). `zero-g models <provider>` lists models and `zero-g status [provider]` shows authentication status. Commands share the configured database and encryption key.

Existing plaintext Antigravity and Codex credential files are imported once into encrypted database storage when no stored credential exists. The importer checks `ZERO_G_CREDENTIALS_AGY` (or `ZERO_G_CREDENTIALS`) and `ZERO_G_CREDENTIALS_CODEX` overrides respectively, then a `credentials-agy.json` or `credentials-codex.json` in the working directory, then the corresponding file under `$XDG_CONFIG_HOME/zero-g` (or `~/.config/zero-g`). Make the files available before the first provider load; an absent file is also recorded as an import attempt. Imported files are retained, not deleted; protect or securely dispose of them yourself after confirming the migration. Subsequent login and refresh use the database.

The database contains sensitive full request and response bodies as well as settings and encrypted secrets. Restrict filesystem access, protect backups, and use TLS for dashboard access. Token counts are recorded only when the provider includes usage in its response.
