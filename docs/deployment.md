# Deployment & Operations

## Install and initial setup

Install with `brew install Delnegend/tap/zero-g`, download a [release](https://github.com/Delnegend/zero-g/releases/latest), or build from checkout with `(cd frontend && bun install --frozen-lockfile) && just frontend-build && cargo install --path .`.

Copy `.env.example` to `.env`, set `ZERO_G_ENCRYPTION_KEY` to the output of `openssl rand -base64 32`, and set a strong dashboard password. Keep this key unchanged across restarts and backups. Protect `.env` from other users. Run `zero-g`, sign in at `http://127.0.0.1:8080/`, and create entries under **Providers**. Configure Google OAuth under **Settings** before adding a credential and signing in to an Antigravity entry; see [configuration](configuration.md#google-oauth). For Codex, add a credential and start device sign-in from its entry. Create and copy a virtual key under **Virtual keys**, then use it with the shared `/v1` API and provider-prefixed model IDs.
Set `ZERO_G_DASHBOARD_ORIGIN` to the exact browser-facing origin: `http://127.0.0.1:8080` for local provisioning, or the public HTTPS origin behind a reverse proxy. Update it when changing the dashboard origin.


Use the virtual key as the bearer token for every inference request. Basic or OIDC protects the dashboard separately; use TLS outside localhost. The default listening address is loopback. Set `ZERO_G_BIND` in the service environment and restart when a different address is needed. Previously stored dashboard bind settings are ignored; set `ZERO_G_BIND` before upgrading if the old address must be retained.

## Run as a service

For Homebrew, `brew services start zero-g` runs the installed service. Ensure the service environment supplies the same required encryption key, database path and dashboard authentication settings; an interactive shell export alone need not reach a service.

For a systemd user service, create `~/.config/systemd/user/zero-g.service`, adjusting the executable and environment file paths:

```ini
[Unit]
Description=zero-g AI gateway
After=network.target

[Service]
EnvironmentFile=%h/.config/zero-g/service.env
ExecStart=/usr/local/bin/zero-g
Restart=on-failure

[Install]
WantedBy=default.target
```

Put the bootstrap variables in the environment file, restrict it to the service user, and use an absolute `ZERO_G_LOG_DB` path. Run `systemctl --user daemon-reload` and `systemctl --user enable --now zero-g`; inspect logs with `journalctl --user -u zero-g -f`.

## Container deployment

The supplied Compose configuration builds a non-root distroless image, persists `/app/data` in the `zero-g-data` volume, and publishes port 8080 only on host loopback. Copy `.env.example` to `.env`, set the stable encryption key and strong Basic credentials, and restrict its permissions. Migrations are embedded in the binary and applied at startup; no migration files are required in the runtime image.

Compose defaults `ZERO_G_BIND` to `0.0.0.0:8080` inside the container so the host port can reach it; the published port remains restricted to host loopback. If `.env` specifies `ZERO_G_BIND`, that value overrides the Compose default and must be reachable from the container network.

Run `docker compose up -d`, sign in at `http://127.0.0.1:8080/`, create and copy a virtual key, and add a Zen provider with URL `http://zen-shim:8090/v1` under **Providers**. Use a protected TLS reverse proxy if remote access is needed.

Use the dashboard **Providers** page for sign-in in containers, including remote Google callback URL paste and Codex device authorization. Do not run multiple instances against the database while provisioning.

## Upgrade, migration and backup

Before upgrading, stop zero-g and back up the entire database and associated sidecar files, plus the encryption key stored separately and securely. The database holds private request/response bodies and credentials; the key is required to decrypt its secrets. Retain a protected copy of the old database for rollback. The embedded Toasty/Turso migrations run on startup; verify the upgraded service and provider sign-in before retiring the backup.

Older singleton credentials and key restrictions are not migrated to named providers. For those installations, create provider entries and sign in again, then create virtual keys for the intended provider IDs and update clients to use the shared `/v1` API.

On upgrading an existing named-provider database, built-in provider IDs are migrated to their fixed IDs, along with credential ownership and virtual-key provider restrictions. Update clients using old provider-prefixed model IDs. Startup refuses the migration if a destination ID is occupied, multiple active entries have the same built-in kind, or a key already grants the destination without granting the old ID; reconcile the reported conflict before retrying.

