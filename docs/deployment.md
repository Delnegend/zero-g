# Deployment & Operations

## Container deployment

Zero-G releases are `linux/amd64` container images at `ghcr.io/delnegend/zero-g`. The supplied Compose file pulls `latest`, persists `/app/data` in the `zero-g-data` volume, and publishes port 8080 only on host loopback. Use a version tag in `compose.yml` to pin a release.

The current `latest` image is 0.5.8 and uses the older CLI. The setup below applies after the next container image is published.

Download `compose.yml` and `.env.example` from the project, save the latter as `.env`, and restrict its permissions. Set `ZERO_G_ENCRYPTION_KEY` to the output of `openssl rand -base64 32`, keep it stable and back it up separately, and set a strong dashboard password. See the [quick start](../README.md#quick-start) for commands.

Run `docker compose pull` and `docker compose up -d`, then check `curl --fail http://127.0.0.1:8080/health`. Sign in at `http://127.0.0.1:8080/`, create a provider and credential under **Providers**, then create and copy a virtual key under **Virtual keys**. Supply that key as the bearer token for inference requests.

Compose defaults `ZERO_G_BIND` to `0.0.0.0:8080` inside the container; the published port remains restricted to host loopback. If `.env` sets `ZERO_G_BIND`, it must be reachable from the container network. Set `ZERO_G_DASHBOARD_ORIGIN` to the exact browser-facing origin, and use HTTPS outside localhost.

Use `docker compose logs zero-g` to inspect logs. For updates, back up the database and encryption key, then run `docker compose pull` and `docker compose up -d`. Migrations are embedded in the image and applied at startup. Use the dashboard for provider sign-in, including remote Google callback URL paste and Codex device authorization.

## Upgrade, migration and backup

Before upgrading, stop zero-g and back up the entire database and associated sidecar files, plus the encryption key stored separately and securely. The database holds private request/response bodies and credentials; the key is required to decrypt its secrets. Retain a protected copy of the old database for rollback. The embedded Toasty/Turso migrations run on startup; verify the upgraded service and provider sign-in before retiring the backup.

Older singleton credentials and key restrictions are not migrated to named providers. For those installations, create provider entries and sign in again, then create virtual keys for the intended provider IDs and update clients to use the shared `/v1` API.

On upgrading an existing named-provider database, built-in provider IDs are migrated to their fixed IDs, along with credential ownership and virtual-key provider restrictions. Update clients using old provider-prefixed model IDs. Startup refuses the migration if a destination ID is occupied, multiple active entries have the same built-in kind, or a key already grants the destination without granting the old ID; reconcile the reported conflict before retrying.

