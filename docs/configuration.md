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
| `ZERO_G_OIDC_REDIRECT_URL` | None | Required in OIDC mode: the dashboard origin followed by `/auth/callback` |

Register the exact redirect URL with the identity provider. OIDC sessions expire after eight hours and are lost on restart. Dashboard authentication is required; use HTTPS outside localhost.

On HTTPS, OIDC uses `__Host-` session and login-state cookies with `Secure`, `HttpOnly`, and `Path=/`, without a `Domain` attribute. Browsers that enforce the prefix prevent sibling subdomains from setting those cookies. Loopback HTTP uses unprefixed cookies because it cannot reliably use `Secure`. If the same cookie name appears more than once, zero-g uses the last value; duplicate cookies can therefore still cause login failures or session confusion. Existing HTTPS sessions require signing in again after upgrading.

Protected dashboard responses use `Cache-Control: no-store`. Up to 1,024 OIDC login attempts can be pending at once; additional attempts receive HTTP 503 until a pending attempt completes or expires.

At least one allowed email or group is required. If both are configured, a verified email match **or** a group match grants access. Group membership is checked in the signed ID token; the provider must support the `groups` scope. Group names are case-sensitive and may differ from display names. Sessions last eight hours, so membership changes do not invalidate existing sessions until they expire or the server restarts.

## Dashboard settings and virtual keys

Sign in to the dashboard and open **Settings** to configure log retention (default 30 days, range 1–36500). A saved change takes effect immediately and removes records older than the new limit.

The Dashboard's date picker defaults to the last 24 hours and offers presets for 1 hour, 6 hours, 7 days, 1 month, 6 months and 1 year. Select calendar dates and choose **Apply dates** for a custom range, including both full days in your browser's local time zone. Filter graphs by provider, model, request path and success or error status. Graphs include all retained matching requests, not just the latest 200 shown in **LLM Logs**. Time graphs group requests by UTC hour for ranges up to one day, day up to three calendar months, month up to three calendar years, and year for longer ranges. Only periods with requests are shown. Token usage is shown only when reported by the provider. Preset months and years use calendar dates. Longer ranges cannot show requests already removed by log retention.

The dashboard shows summary cards for total requests, successful requests, tokens, and average latency. Input and output tokens have separate graphs. Except for model usage, graphs compare the selected range with the immediately preceding equal-length range using the same filters. Dashed lines align buckets by position within their selected ranges, preserving gaps. Previous-period lines do not show tooltips. Partial edge buckets can cover different durations. Comparison data is unavailable when the earlier range falls outside active log retention or cannot be loaded. Retained history can still be incomplete, for example after increasing retention.

Summary-card trends compare each value with the preceding range. Green indicates an increase in requests, successes or tokens, or a decrease in latency; red indicates the opposite. An increase from zero is shown as “New” rather than a percentage. Trends are omitted when the comparison or a required value is unavailable.

Graph headers show the percentage change without a color judgment. Hover over or focus the indicator for the previous value and any comparison limitations. An increase from zero is shown as “New”; a dash means the comparison is unavailable.

Request volume bars stack successful and failed requests for each period. Model usage bars stack the models for each period and do not show a previous-period comparison.

Use the filter sidebar on the Dashboard or LLM Logs to select one or more providers, models, request paths, response statuses, virtual keys or provider credentials. Options within a group match any selection; groups combine to narrow the results. LLM Logs loads the latest 200 requests after applying these filters; its text search and sorting work within those results. Reset clears the filters without changing the date range. Filters stay in each page address for sharing and browser navigation; the Dashboard date range remains local to the current page.

The filter sidebar stays visible while scrolling; on small screens, the **Filters** button stays visible instead. Long request paths scroll inside the filter while visible. Scrolling respects the device's reduced-motion setting.

Request log details show the authenticated virtual key and the provider credential that successfully served the request. Requests without a successful provider credential and older logs show **Unknown** where the identity is unavailable. Requests without a valid virtual key are rejected before their body is read and are not logged. Names are stored as they appeared when the request was handled, so filters continue to work after a key is revoked.

Request log rows show the status, request type, model, latency and input/output tokens. Select a row or its timestamp on desktop, or **View** on mobile, to open request details; unavailable token counts are shown as unknown.

Under **Models → Providers**, create one instance of each built-in provider and as many OpenAI-compatible providers as needed for different upstream URLs. Built-in IDs are `agy`, `codex`, `claude`, `cursor`, `grok`, `grok-build`, `zen`, `go` and `openrouter`; custom providers have user-selected IDs and names. All providers share the client base URL `http://127.0.0.1:8080/v1`. Configure a base URL for OpenAI-compatible providers, then add named credentials under the provider. An empty API key explicitly selects a keyless upstream for OpenAI-compatible providers and Zen; OpenCode Go and OpenRouter require API keys and use their official endpoints. Secrets are encrypted at rest. OpenAI-compatible, Go, Zen and OpenRouter Chat Completions and Responses are forwarded to their matching upstream endpoints. For Go, pass the client's stable `x-opencode-session` header with requests.

Zen uses OpenCode's official model list and API for all models, including IDs ending in `:free`. Models requiring other native APIs, such as Jev's System One API, are not translated.

Each enabled, connected credential has a positive weight: a weight of 3 receives roughly three times as many initial requests as a weight of 1. On upstream authentication errors (401 or 403) or rate limits (429), zero-g tries another credential before returning a response. It does not retry after streaming has begun.

Antigravity, Codex and Claude credential cards show the account email when the provider supplied one during sign-in. Codex also shows its subscription plan when present in the stored token claims; it updates when credentials are refreshed or reconnected and may be stale after a subscription change. If an older Codex credential has no stored email or plan, reconnect it to obtain the current account information.

Credential cards show remaining quota as bars and reset countdowns for Codex, Antigravity, Claude Code and OpenCode Go. Hover over or focus a countdown to see the exact local date and time. Unavailable data is shown as unknown or an error rather than an estimated quota. Antigravity shows quota windows under model-pool headings, with five-hour windows before longer limits; if the summary is unavailable, the card shows an error. Claude Code uses Anthropic's undocumented subscription usage endpoint, which may change or return rate-limit errors. Usage is cached for five minutes; use **Refresh usage** to request a new reading. If refresh fails, the previous reading remains visible with a retry option.

Zen and OpenRouter configuration buttons appear alongside the credential's edit and delete actions.

Zen balance requires a separate Console login: select **Configure console** on its credential and enter the workspace ID and the value of the `__Host-console_session` cookie. In your browser's developer tools on `opencode.ai`, find the cookie under storage and the workspace ID in the `x-org-id` header of the Console billing request. Treat the session like a password: it grants access to your Console account, is stored encrypted, expires, and can be removed or replaced in the same dialog. An inference API key alone cannot retrieve the balance.

OpenRouter requires an inference API key for requests. To show account credits, select **Configure management key** on the credential and enter a separate [management key](https://openrouter.ai/docs/guides/overview/auth/management-api-keys); an inference key cannot retrieve the account balance. Management keys have broad account permissions, are stored encrypted, and can be removed in the same dialog. The balance is total credits purchased minus total credits used, not the inference key's spending limit.

**Upgrading an existing database:** Back it up first. Built-in providers with older IDs are renamed to their fixed IDs, and their credentials and virtual-key restrictions are updated. Client URLs change: update clients to use the new URLs. Migration stops if a fixed ID is already occupied or a virtual key has a conflicting restriction; resolve the reported conflict before upgrading. Existing provider credentials are also moved into pools and duplicate providers consolidated; migration refuses merges that would widen virtual-key access or combine Zen providers with different base URLs.

Create a key under **Virtual keys**, copy it from the dialog before dismissing it (it cannot be retrieved later), and supply `Authorization: Bearer <key>` to inference and model endpoints. Select provider IDs and upstream model IDs to restrict access; selecting none permits all. Optionally set an expiration date when creating a key, or change or clear it later. Dates and times use the browser's local time zone; selecting a date defaults to 23:59:59, which can be changed. Expired keys cannot be used until their expiration is extended or removed. Revoke compromised keys in the same screen; revocation is permanent, immediately prevents new requests, and leaves the key's metadata visible. Dashboard authentication and virtual keys are separate controls.

Use model IDs in the form `<provider-id>/<upstream-model-id>`, as returned by `/v1/models`, for both Chat Completions and Responses. Upstream model IDs may themselves contain slashes. The old provider-specific URLs no longer work; update clients to the shared `/v1` base URL and namespaced model IDs. Virtual-key model restrictions use the upstream model ID without the provider prefix. Response bodies and streams do not add the provider prefix to their model fields.

For OpenRouter and other OpenAI-compatible providers, a restricted key must also permit every model in a `models` fallback list. Unrestricted keys retain provider-specific fallback behavior.

Model discovery queries permitted providers concurrently. If some providers fail, the response includes their IDs in an `errors` field alongside successful results; if all fail, it returns an error. Providers that do not support a requested API still return an error for that API.

For Codex, Antigravity, Claude, Cursor and Grok, send a stable conversation ID in `session_id` or `x-session-id` to retain upstream session affinity across turns. Accepted headers, in precedence order, are `x-bf-eh-session_id`, `session_id`, `session-id`, `x-session-id`, `x-opencode-session`, `conversation_id` and `conversation-id`. IDs must be 1–128 visible ASCII characters without whitespace; missing, invalid or duplicate values start a fresh session. IDs are scoped to the virtual key and provider so different keys do not share an upstream session.

Antigravity and Claude responses may include `provider_parts`; Antigravity tool calls may also include `thought_signature`. Clients should return these fields unchanged on later turns to preserve ordered upstream reasoning signatures.

For compatible Antigravity models, Chat Completions accepts inline WAV or MP3 audio as `{"type":"input_audio","input_audio":{"format":"wav","data":"<base64>"}}`, and PDFs as `{"type":"file","file":{"filename":"document.pdf","file_data":"data:application/pdf;base64,<base64>"}}`. Responses accepts the same PDF data as `{"type":"input_file","filename":"document.pdf","file_data":"data:application/pdf;base64,<base64>"}`. For video, use `{"type":"video_url","video_url":{"url":"data:video/mp4;base64,<base64>"}}` or `{"type":"input_video","video_url":"data:video/mp4;base64,<base64>"}`. These are inline inputs; external URLs and uploaded file IDs are not supported for Antigravity.

Inference requests are limited to 20,000,000 bytes including JSON and base64 encoding. Authenticated request logs retain the full request body, including embedded media, but at most the first 256 KiB of the response. Response forwarding is not truncated. Token usage is also read from later streaming events when their data lines are no larger than 256 KiB; usage in longer non-streaming responses or multiline streaming events may be unavailable. Consider the privacy and disk-space implications before sending sensitive or large files.

Remove every credential, including disabled ones, before deleting its provider. Custom provider IDs cannot be reused. A deleted built-in provider can be recreated only after revoking any restricted virtual key that still references its ID; this prevents an old key from silently gaining access to the replacement.

**Models → Catalog** shows discovered models as cards grouped by provider, with available context, output limits and pricing. Antigravity models with matching low, medium or high reasoning variants share one model ID and display their available efforts as badges; the unified `/v1/models` endpoint exposes the same normalized model with its provider prefix. Search by model or provider, filter by provider, and expand a card's details for syntax-highlighted metadata and datasheets. Descriptions are limited to three lines; hover or focus to see the full text. Each provider's models appear in a swipeable row with navigation controls; a faded edge indicates more cards in that direction. A dash means the information is unavailable. Refresh the catalog to fetch models immediately; failed providers retain their last successful list and show an error. Zen uses OpenCode's model endpoint.

The number of visible model cards adapts to the space available, including when the sidebar is open. Long model names, IDs and API labels scroll in a continuous loop with a pause while their cards are visible; hovering reveals the full text. Scrolling is disabled when your device requests reduced motion.

While dashboard data loads, placeholders follow the layout of the corresponding charts, tables, cards, and settings forms. The final size can vary with the data returned.

Input types use icons for text, files, images, audio, video and PDFs; hover over an icon to see its label. Unrecognized input types remain visible as text.

Antigravity input icons reflect the model's advertised capabilities, including enabled MIME types. Actual media support depends on the selected model and upstream service.

OpenCode Zen and Go supply their own model limits, capabilities and base token prices. Their prices can differ for the same model; Go's rates count against its subscription allowance and can have time-based multipliers. Zen may fill missing details from the community-maintained [Models.dev](https://models.dev) catalog, cached for one hour. Missing information remains unknown.

OpenRouter supplies model descriptions, limits, input types, supported parameters and base token prices. Actual prices can vary with provider routing, pricing overrides and non-token charges.

OpenRouter discovery includes text and Decisions models. Cards identify models that use the Decisions API; send those requests to `/v1/decisions`, not Chat Completions or Responses. See the [gateway guide](gateways.md) for an example and input limits.

Claude model details come from the authenticated Anthropic model list, supplemented by exact Anthropic-owned records in the configured datasheets. Anthropic API prices are reference prices, not Claude subscription charges. PDF support shown in the catalog describes the model; Zero-G currently forwards only text and images to Claude.

Codex input types come from its model list when provided, otherwise from provider-specific or exact OpenAI-owned records in the configured datasheets. Matching OpenAI API token prices are informational references, not Codex subscription charges. Models without a matching record show no reference price.

For Antigravity, matching Gemini and Claude API prices from the configured datasheet are shown as a reference, not as Antigravity charges. Matching includes Gemini preview entries and Claude thinking variants when their base model is available. Prices can differ from the actual plan or credit overage costs; models without a matching price show a dash.

The dashboard receives model details and prices from `/api/model-catalog`; the backend combines cached provider results with the latest cached datasheets. If a cached datasheet cannot be read, the catalog remains visible and shows an error; datasheet-based details are unavailable until it is fixed.

For a normalized Antigravity model, pass its base ID and set `reasoning_effort` to an advertised effort. If omitted, zero-g chooses an accessible base model, then medium, low or high. A virtual key restricted to the base ID permits its variants; a key restricted to a variant permits only that variant. Resolving an unsuffixed Antigravity request checks the discovered model list, cached for up to five minutes per credential and project.

**Models → Settings** configures pricing and model-parameter datasheet URLs and automatic refresh intervals. Defaults use Bifrost's public datasheets, synced every 24 hours; provider discovery runs every 60 minutes. Set either interval to zero to disable that automatic task, or use the manual buttons. Leave a URL blank to restore its default. Datasheets are cached in the database; a failed sync keeps the last successful pair. Pricing is informational and is not a billed-cost record.

## Provider sign-in

OAuth application credentials for Antigravity, Codex, Claude, Cursor and Grok are built into the binary. These shared application credentials are public and may be revoked by their providers. Your account tokens are stored separately and encrypted in the database.

Previously configured Google OAuth client settings remain in existing databases but are no longer used. The previously stored client secret remains encrypted.

Create an Antigravity provider under **Models → Providers**, add a credential, and start Google sign-in for that credential. When your browser is on the server machine, the local callback can complete sign-in automatically. If the browser is on another machine, copy the entire redirected loopback URL from its address bar and paste it into the dashboard, even if the loopback page does not load. The URL contains a temporary authorization code; keep it private. For Codex, add a credential to its provider, start sign-in, open the device authorization link, and click the displayed code to copy it. Repeat for each account.

Grok and Grok Build also use a device code: click the displayed code to copy it, then enter it at the linked authorization page.

For Claude Code, open the authorization link and paste the returned code or callback URL. Anthropic's [authentication terms](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use) prohibit third-party applications from routing Claude subscription credentials; use of this integration may violate those terms and may stop working. Cursor uses an undocumented agent protocol; the gateway never executes upstream shell or filesystem requests. Cursor supports `tool_choice: none` and `auto`; named and `required` choices return an error.

`grok` sends OAuth requests to `api.x.ai`, while `grok-build` uses the Grok CLI proxy; they are separate providers and never fall back to one another. Their quotas and billing may differ. The Grok Build proxy is undocumented and its model discovery depends on upstream support. Grok Chat returns an error for unsupported output formats, stop sequences, strict tools, non-text tool results and image detail settings rather than silently dropping them.

The provider implementations refer to [OMP's provider registry](https://github.com/can1357/oh-my-pi/tree/main/packages/catalog/src/compat/rules/auth) and [T3 Code's provider integrations](https://github.com/pingdotgg/t3code/tree/main/packages).

## Running and credentials

Run `zero-g` to start the server. Use the dashboard **Models → Providers** page for provider sign-in and status. Account credentials are read from the database; they are not imported from environment variables or plaintext credential files. Discover models through the authenticated `/v1/models` endpoint.

The API reference at `/redoc` and its OpenAPI document at `/api/openapi.json` require dashboard authentication. ReDoc's JavaScript is bundled with zero-g; it does not require a third-party CDN.

The database contains sensitive full request and response bodies as well as settings and encrypted secrets. Restrict filesystem access, protect backups, and use TLS for dashboard access. Token counts are recorded only when the provider includes usage in its response.

### Exporting provider credentials

Stop zero-g before exporting so the database is not changing. From the project root, run:

```bash
bun run scripts/export-credentials.ts ~/zero-g-credentials.json
```

Bun reads `ZERO_G_LOG_DB` and `ZERO_G_ENCRYPTION_KEY` from the environment or the project's `.env`. The script opens the database read-only, verifies the encryption key, and writes provider API keys, OAuth credentials and auxiliary account secrets as JSON. OAuth credentials are JSON strings in the `value` field. Virtual keys cannot be exported because only their hashes are stored.

Encrypted entries must have a valid 16-byte authentication tag; truncated or altered entries are rejected without writing an export.

The output contains **plaintext secrets**. The script creates a new file with owner-only permissions on POSIX systems and refuses to overwrite an existing file. Store it securely, avoid placing it in the repository or logs, and delete it when no longer needed.

