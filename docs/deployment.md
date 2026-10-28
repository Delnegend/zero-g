# Deployment & Operations

## Install and initial setup

Install with `brew install Delnegend/tap/zero-g`, download a [release](https://github.com/Delnegend/zero-g/releases/latest), or build from checkout with `(cd frontend && bun install --frozen-lockfile) && just frontend-build && cargo install --path .`.

Copy `.env.example` to `.env`, set `ZERO_G_ENCRYPTION_KEY` to the output of `openssl rand -base64 32`, and set a strong dashboard password. Keep this key unchanged across restarts and backups. Protect `.env` from other users. Run `zero-g`, sign in at `http://127.0.0.1:8080/`, and create and copy a virtual key under **Virtual keys**. Configure Google OAuth and other settings under **Settings**. For Antigravity, create a Desktop OAuth client as described in [configuration](configuration.md#google-oauth), then use **Providers** to start Google sign-in. A local browser can complete the callback automatically; on another machine, paste the entire redirected loopback URL into the dashboard even if it fails to load. Keep this URL private. For Codex, use the **Providers** device sign-in link and enter the displayed code.
Set `ZERO_G_DASHBOARD_ORIGIN` to the exact browser-facing origin: `http://127.0.0.1:8080` for local provisioning, or the public HTTPS origin behind a reverse proxy. Update it when changing the dashboard origin.


Use the virtual key as the bearer token for every inference request. Basic or OIDC protects the dashboard separately; use TLS outside localhost. The default listening address is loopback. Set `ZERO_G_BIND` in the service environment and restart when a different address is needed. Previously stored dashboard bind settings are ignored; set `ZERO_G_BIND` before upgrading if the old address must be retained.

## Run as a service

For Homebrew, `brew services start zero-g` runs the installed service. Ensure the service environment supplies the same required encryption key, database path and dashboard authentication settings; an interactive shell export alone need not reach a service.

For a systemd user service, create `~/.config/systemd/user/zero-g.service`, adjusting the executable and environment file paths:

```ini
[Unit]
Description=zero-g proxy
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

Run `docker compose up -d`, sign in at `http://127.0.0.1:8080/`, create and copy a virtual key, and set the Zen URL to `http://zen-shim:8090/v1` in Settings. Use a protected TLS reverse proxy if remote access is needed.

Use the dashboard **Providers** page for sign-in in containers, including remote Google callback URL paste and Codex device authorization. Do not run multiple instances against the database while provisioning.

## Upgrade, migration and backup

Before upgrading, stop zero-g and back up the entire database and associated sidecar files, plus the encryption key stored separately and securely. The database holds private request/response bodies and credentials; the key is required to decrypt its secrets. Retain a protected copy of the old database for rollback. The embedded Toasty/Turso migrations run on startup; verify the upgraded service and provider sign-in before retiring the backup.

For a database from the previous libSQL-based release, do not point the new server directly at the old file. Stop all writers, checkpoint the database so no source `-wal` file remains, and retain the backup. The separate offline `zero-g-import-legacy` utility is included alongside `zero-g` in the [release archives](https://github.com/Delnegend/zero-g/releases). Import the existing source into a new, nonexistent destination:

```bash
zero-g-import-legacy /path/to/old.db /path/to/new.db
```

For the container image, override the entrypoint and mount the source and destination directories so the non-root container user can access them:

```bash
docker run --rm --entrypoint /usr/local/bin/zero-g-import-legacy \
  -v /path/to/database-directory:/data \
  ghcr.io/delnegend/zero-g:latest /data/old.db /data/new.db
```

The source is retained. Check the destination before setting `ZERO_G_LOG_DB` to its path and starting the upgraded server. If import fails, discard the incomplete destination before retrying. The importer copies logs and any stored settings, encrypted secrets and virtual-key hashes; retain the original encryption key for existing encrypted secrets.

Legacy plaintext provider files are imported once and left in place; make them visible at their documented paths before first provider load. Remove or secure those plaintext files yourself after confirming import. Existing provider-token environment variables no longer protect inference: create virtual keys and update every client or gateway to use them before switching traffic. See [configuration](configuration.md) for import paths and dashboard authentication.
