# Deployment & Operations

## Install and initial setup

Install with `brew install Delnegend/tap/zero-g`, download a [release](https://github.com/Delnegend/zero-g/releases/latest), or build from checkout with `(cd web && bun install --frozen-lockfile) && just web-build && cargo install --path .`.

Copy `.env.example` to `.env`, set `ZERO_G_ENCRYPTION_KEY` to the output of `openssl rand -base64 32`, and set a strong dashboard password. Keep this key unchanged across restarts and backups; all commands need it. Protect `.env` from other users. Run `zero-g serve`, sign in at `http://127.0.0.1:8080/`, and create and copy a virtual key under **Virtual keys**. Configure Google OAuth and other settings under **Settings**. For Antigravity, create a Desktop OAuth client as described in [configuration](configuration.md#google-oauth), then use **Providers** to start Google sign-in. A local browser can complete the callback automatically; on another machine, paste the entire redirected loopback URL into the dashboard even if it fails to load. Keep this URL private. For Codex, use the **Providers** device sign-in link and enter the displayed code. Alternatively, run `zero-g login agy` or `zero-g login codex` (or `zero-g login codex --device`) with the same database and encryption key.
Set `ZERO_G_DASHBOARD_ORIGIN` to the exact browser-facing origin: `http://127.0.0.1:8080` for local provisioning, or the public HTTPS origin behind a reverse proxy. Update it when changing the dashboard origin.


Use the virtual key as the bearer token for every inference request. Basic or OIDC protects the dashboard separately; use TLS outside localhost. The default bind is loopback. Change it in Settings and restart only when network access is needed.

## Run as a service

For Homebrew, `brew services start zero-g` runs the installed service. Ensure the service environment supplies the same required encryption key, database path and dashboard authentication settings; an interactive shell export alone need not reach a service.

For a systemd user service, create `~/.config/systemd/user/zero-g.service`, adjusting the executable and environment file paths:

```ini
[Unit]
Description=zero-g proxy
After=network.target

[Service]
EnvironmentFile=%h/.config/zero-g/service.env
ExecStart=/usr/local/bin/zero-g serve
Restart=on-failure

[Install]
WantedBy=default.target
```

Put the bootstrap variables in the environment file, restrict it to the service user, and use an absolute `ZERO_G_LOG_DB` path. Run `systemctl --user daemon-reload` and `systemctl --user enable --now zero-g`; inspect logs with `journalctl --user -u zero-g -f`.

## Container deployment

The supplied Compose configuration builds a non-root distroless image, persists `/app/data` in the `zero-g-data` volume, and publishes port 8080 only on host loopback. Copy `.env.example` to `.env`, set the stable encryption key and strong Basic credentials, and restrict its permissions. Migrations are embedded in the binary and applied at startup; no migration files are required in the runtime image.

A new database binds to `127.0.0.1:8080` *inside* the container, so ordinary port publishing cannot reach its dashboard until the persisted bind is changed. On Linux, provision it privately using the same Compose volume and host networking:

```bash
# Build the image and create the persistent volume without starting the server.
docker compose build zero-g
docker volume create zero-g-data
# Run on host loopback; stop any host server on port 8080 first.
docker run --rm -it --network host --env-file .env \
  -v zero-g-data:/app/data ghcr.io/delnegend/zero-g:latest
```

Sign in at `http://127.0.0.1:8080/`, create and copy a virtual key, and set the bind to `0.0.0.0:8080` and Zen URL to `http://zen-shim:8090/v1` in Settings. Stop the temporary server, then run `docker compose up -d`. The regular published port remains host-loopback-only; use a protected TLS reverse proxy if remote access is needed. Host networking differs on non-Linux platforms: instead provision the same database with a local zero-g process, or use a private tunnel into the container network. Do not expose an unauthenticated dashboard to provision it.

Use the dashboard **Providers** page for sign-in in containers, including remote Google callback URL paste and Codex device authorization. If using CLI login instead, run an appropriately networked one-off container with the same persistent volume, environment and encryption key; use `login codex --device` for headless Codex login. Do not run multiple instances against the database while provisioning.

## Upgrade, migration and backup

Before upgrading, stop zero-g and back up the entire database and associated sidecar files, plus the encryption key stored separately and securely. The database holds private request/response bodies and credentials; the key is required to decrypt its secrets. Retain a protected copy of the old database for rollback. The embedded Toasty/Turso migrations run on startup; verify the upgraded service and provider sign-in before retiring the backup.

For a database from the previous libSQL-based release, do not point the new server directly at the old file. Stop all writers, checkpoint the database so no source `-wal` file remains, and retain the backup. Import the existing source into a new, nonexistent destination:

```bash
zero-g import-legacy /path/to/old.db /path/to/new.db
```

The source is retained. Check the destination before setting `ZERO_G_LOG_DB` to its path and starting the upgraded server. If import fails, discard the incomplete destination before retrying. For the container image, pass `import-legacy SOURCE DESTINATION` as the container command; the paths and their directories must be mounted and accessible to the non-root container user. The importer copies logs and any stored settings, encrypted secrets and virtual-key hashes; retain the original encryption key for existing encrypted secrets.

Legacy plaintext provider files are imported once and left in place; make them visible at their documented paths before first provider load. Remove or secure those plaintext files yourself after confirming import. Existing provider-token environment variables no longer protect inference: create virtual keys and update every client or gateway to use them before switching traffic. See [configuration](configuration.md) for import paths and dashboard authentication.
