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
# edit .env with your OAUTH_CLIENT_ID and OAUTH_CLIENT_SECRET
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

Inspect service logs:

```bash
tail -f "$(brew --prefix)/var/log/zero-g.log"
```

### Systemd User Unit (Linux)

Create `~/.config/systemd/user/zero-g.service`:

```ini
[Unit]
Description=Google Antigravity OpenAI Proxy
After=network.target

[Service]
ExecStart=%h/.cargo/bin/zero-g serve
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

## Upgrade

```bash
brew upgrade zero-g
```
