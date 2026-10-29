# Configuration Reference

## Bootstrap environment

The server requires `ZERO_G_ENCRYPTION_KEY`: standard Base64 encoding of exactly 32 random bytes. Generate it once with `openssl rand -base64 32`, keep it stable, and back it up securely. Changing or losing it makes encrypted secrets unreadable. The application also reads a `.env` file in its working directory; protect that file.

| Variable | Default | Purpose |
|---|---|---|
| `ZERO_G_ENCRYPTION_KEY` | None | Required encryption key for stored secrets |
| `ZERO_G_LOG_DB` | `zero-g-logs.db` | Local Turso database path; container default `/app/data/zero-g-logs.db` |
| `ZERO_G_BIND` | `127.0.0.1:8080` | Listening IP address and port; changes take effect on restart |
| `ZERO_G_DASHBOARD_AUTH` | None | Required for the server: `basic` or `oidc` |
| `ZERO_G_DASHBOARD_ORIGIN` | None | Required for the server in every mode: exact dashboard origin, HTTPS except loopback HTTP locally; protects dashboard writes |
| `ZERO_G_DASHBOARD_USERNAME`, `ZERO_G_DASHBOARD_PASSWORD` | None | Required in Basic mode; username cannot contain `:` |
| `ZERO_G_OIDC_ISSUER`, `ZERO_G_OIDC_CLIENT_ID`, `ZERO_G_OIDC_CLIENT_SECRET` | None | Required in OIDC mode |
| `ZERO_G_OIDC_ALLOWED_EMAILS` | None | OIDC comma-separated allowlist of verified email addresses |
| `ZERO_G_OIDC_ALLOWED_GROUPS` | None | OIDC comma-separated allowlist of exact group names, for example `admins` |
| `ZERO_G_OIDC_REDIRECT_URL` | None | OIDC callback URL ending in `/auth/callback`: HTTPS except loopback HTTP for local development |

Register the exact redirect URL with the identity provider. OIDC sessions expire after eight hours and are lost on restart. Never disable dashboard authentication on an untrusted network; use HTTPS outside localhost.
At least one allowed email or group is required. If both are configured, a verified email match **or** a group match grants access. Group membership is checked in the signed ID token; the provider must support the `groups` scope. Group names are case-sensitive and may differ from display names. Sessions last eight hours, so membership changes do not invalidate existing sessions until they expire or the server restarts.

## Dashboard settings and virtual keys

Sign in to the dashboard and open **Settings** to configure log retention (default 30 days, range 1–36500) and the Google OAuth desktop client ID and secret.

The Dashboard's date picker defaults to the last 24 hours and offers presets for 7 days, 1 month, 6 months and 1 year. Select calendar dates and choose **Apply dates** for a custom range, including both full days in your browser's local time zone. Totals and the chart include all retained requests in the selected range; the request list shows the latest 200. The chart groups requests by UTC hour for ranges up to one day, day up to three calendar months, month up to three calendar years, and year for longer ranges. Only periods with requests are shown. Preset months and years use calendar dates. Longer ranges cannot show requests already removed by log retention.

Request log rows show the status, request type, model, latency and input/output tokens. Select a row or its timestamp on desktop, or **View** on mobile, to open request details; unavailable token counts are shown as unknown.

Under **Models → Providers**, create one Antigravity, Codex, Zen or OpenCode Go provider per kind, and as many OpenAI-compatible providers as needed for different upstream URLs. Built-in IDs and names are fixed: `agy` / Google Antigravity, `codex` / OpenAI Codex, `zen` / OpenCode Zen, and `go` / OpenCode Go. Custom OpenAI-compatible providers have user-selected IDs and names. Each provider's client URL is `http://127.0.0.1:8080/{kind}/{id}/v1`. Configure a base URL for Zen and OpenAI-compatible providers, then add named credentials under the provider. An empty API key explicitly selects a keyless upstream for those providers; OpenCode Go requires an API key and uses its official endpoint by default. Secrets are encrypted at rest. OpenAI-compatible and Go Chat Completions and Responses are forwarded to their matching upstream endpoints. For Go, pass the client's stable `x-opencode-session` header with requests.

Each enabled, connected credential has a positive weight: a weight of 3 receives roughly three times as many initial requests as a weight of 1. On upstream authentication errors (401 or 403) or rate limits (429), zero-g tries another credential before returning a response. It does not retry after streaming has begun.

Credential cards show available quota windows and reset times for Codex, Antigravity and OpenCode Go. Figures are remaining capacity; unavailable data is shown as unknown or an error rather than an estimated quota. The dashboard refreshes usage when opened and caches it for five minutes.

Zen balance requires a separate Console login: select **Configure console** on its credential and enter the workspace ID and the value of the `__Host-console_session` cookie. In your browser's developer tools on `opencode.ai`, find the cookie under storage and the workspace ID in the `x-org-id` header of the Console billing request. Treat the session like a password: it grants access to your Console account, is stored encrypted, expires, and can be removed or replaced in the same dialog. An inference API key alone cannot retrieve the balance.

**Upgrading an existing database:** Back it up first. Built-in providers with older IDs are renamed to their fixed IDs, and their credentials and virtual-key restrictions are updated. Client URLs change: update clients to use the new URLs. Migration stops if a fixed ID is already occupied or a virtual key has a conflicting restriction; resolve the reported conflict before upgrading. Existing provider credentials are also moved into pools and duplicate providers consolidated; migration refuses merges that would widen virtual-key access or combine Zen providers with different base URLs.

Create a key under **Virtual keys**, copy it from the dialog before dismissing it (it cannot be retrieved later), and supply `Authorization: Bearer <key>` to inference and model endpoints. Restrict it to individual provider IDs and models as needed; selecting none permits all. Revoke compromised keys in the same screen. Dashboard authentication and virtual keys are separate controls.

For Codex and Antigravity, send a stable conversation ID in `session_id` or `x-session-id` to retain upstream session affinity across turns. Accepted headers, in precedence order, are `x-bf-eh-session_id`, `session_id`, `session-id`, `x-session-id`, `x-opencode-session`, `conversation_id` and `conversation-id`. IDs must be 1–128 visible ASCII characters without whitespace; missing, invalid or duplicate values start a fresh session. IDs are scoped to the virtual key and provider so different keys do not share an upstream session.

Antigravity responses may include `provider_parts` and tool-call `thought_signature` fields. Clients that support these fields should return them unchanged with the assistant message on later turns to preserve upstream reasoning signatures.

Deleting a provider removes its credentials. Custom provider IDs cannot be reused. A deleted built-in provider can be recreated only after revoking any restricted virtual key that still references its ID; this prevents an old key from silently gaining access to the replacement.

**Models → Catalog** shows discovered models as cards grouped by provider, with available context, output limits and pricing. Antigravity models with matching low, medium or high reasoning variants share one model ID and display their available efforts as badges; the provider's `/v1/models` endpoint exposes the same normalized model. Search by model or provider, filter by provider, and expand a card's details for the original metadata and datasheets. A dash means the information is unavailable. Refresh the catalog to fetch models immediately; failed providers retain their last successful list and show an error. Zen uses the model list advertised by zero-g.

For a normalized Antigravity model, pass its base ID and set `reasoning_effort` to an advertised effort. If omitted, zero-g chooses an accessible base model, then medium, low or high. A virtual key restricted to the base ID permits its variants; a key restricted to a variant permits only that variant. Resolving an unsuffixed Antigravity request checks the discovered model list, cached for up to five minutes per credential and project.

**Models → Settings** configures pricing and model-parameter datasheet URLs and automatic refresh intervals. Defaults use Bifrost's public datasheets, synced every 24 hours; provider discovery runs every 60 minutes. Set either interval to zero to disable that automatic task, or use the manual buttons. Leave a URL blank to restore its default. Datasheets are cached in the database; a failed sync keeps the last successful pair. Pricing is informational and is not a billed-cost record.

## Google OAuth

Create a Desktop app OAuth client in [Google Cloud Console](https://console.cloud.google.com/) under **APIs & Services → Credentials**, with the required `cloud-platform`, `userinfo.email`, `userinfo.profile`, `cclog` and `experimentsandconfigs` scopes. Enter its client ID and secret in Settings.

Create an Antigravity provider under **Models → Providers**, add a credential, and start Google sign-in for that credential. When your browser is on the server machine, the local callback can complete sign-in automatically. If the browser is on another machine, copy the entire redirected loopback URL from its address bar and paste it into the dashboard, even if the loopback page does not load. The URL contains a temporary authorization code; keep it private. For Codex, add a credential to its provider, start sign-in, open the device authorization link, and enter the displayed code. Repeat for each account.

## Running and credentials

Run `zero-g` to start the server. Use the dashboard **Models → Providers** page for provider sign-in and status. Provider credentials and OAuth settings are read from the database; they are not imported from environment variables or plaintext credential files. Discover models through the authenticated provider `/v1/models` endpoints.

The database contains sensitive full request and response bodies as well as settings and encrypted secrets. Restrict filesystem access, protect backups, and use TLS for dashboard access. Token counts are recorded only when the provider includes usage in its response.
