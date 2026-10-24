# Deployment & Operations

## Install

```bash
brew install Delnegend/tap/zero-g
```

For systems without Homebrew, download the binary from [GitHub Releases](https://github.com/Delnegend/zero-g/releases/latest) or install from checkout:

```bash
cargo install --path .
```

## Initial Authentication

Before logging in, configure your OAuth desktop credentials in `.env` (see **[Obtaining OAuth Credentials](configuration.md#obtaining-oauth-credentials)**):

```bash
cp .env.example .env
# edit .env with your ZERO_G_OAUTH_CLIENT_ID and ZERO_G_OAUTH_CLIENT_SECRET
```

Run the sign-in command to authenticate:

```bash
zero-g login
```

After authentication, credentials are saved to `~/.config/zero-g/credentials.json`.
## Run as a Service

### Homebrew Services (macOS & Linux)

```bash
brew services start zero-g
```

Inspect service logs (paths match the Homebrew formula's `service` block):

```bash
tail -f /tmp/zero-g.log
```

### Systemd User Unit (Linux, non-Homebrew installs)

Homebrew users should prefer `brew services start zero-g` above. For installs
outside Homebrew (release binary or `cargo install`), create
`~/.config/systemd/user/zero-g.service` pointing at the installed binary:

```ini
[Unit]
Description=Google Antigravity OpenAI Proxy
After=network.target

[Service]
ExecStart=/usr/local/bin/zero-g serve
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

Enable and start:

```bash
systemctl --user daemon-reload
systemctl --user enable --now zero-g
journalctl --user -u zero-g -f
```

## Container Deployment

`zero-g` ships a static distroless container image based on `gcr.io/distroless/static-debian13:nonroot` (<10 MB), published to GitHub Packages (`ghcr.io/delnegend/zero-g`).

### 1. Seed `credentials.json`

Authenticate and seed credentials using a temporary one-off container:

```bash
mkdir -p /tmp/zero-g && chmod 777 /tmp/zero-g

podman run --rm -it \
  --net=host \
  --env-file .env \
  -v /tmp/zero-g:/home/nonroot/.config/zero-g:Z \
  ghcr.io/delnegend/zero-g:latest login

mv /tmp/zero-g/credentials.json ./credentials.json
chmod 644 ./credentials.json
rm -rf /tmp/zero-g
```

### 2. Run with Compose

```bash
docker compose up -d
# or: podman compose up -d
```

## Upgrade

```bash
brew upgrade zero-g
```
