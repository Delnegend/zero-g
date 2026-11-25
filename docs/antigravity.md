# Antigravity

Zero-G does not route Antigravity requests, because Google's terms of service forbid it. The best it can do is track your usage. This page covers the one program that does that.

## What Zero-G does

Your `agy` CLI talks to Google directly, and Zero-G never sees your Google login.

`zero-g-agy-integration` is a single program with two commands. Both run on the machine where you use `agy`, read what the official CLI already knows, and report it to Zero-G:

- `quota` reports how much of your 5-hour and weekly limits is left and which plan you are on. It appears on the Google Antigravity credential card.
- `observe` is an `agy` hook. It records each model response in the request logs.

Use `quota` if you only want to see your limits. Add the hook if you also want prompts and responses in the logs.

A Google Antigravity provider in the dashboard is only a place to show quota. It has no model routes and does not appear in `/v1/models`.

## Install

The program is released with Zero-G and has the same version number. Each [GitHub release](https://github.com/Delnegend/zero-g/releases) includes static binaries for Linux on x86-64 and arm64. Homebrew installs and updates them:

```bash
brew tap Delnegend/tap
brew install zero-g-agy-integration
```

Without Homebrew, download the archive for your platform and put `zero-g-agy-integration` on your `PATH`.

## Settings

Both commands read the same settings from the environment or from `~/.config/zero-g/agy-integration.env`. Environment variables win, and `--config FILE` points to a different file. A settings file works for a background service and for an `agy` hook, neither of which has your shell profile.

```bash
mkdir -p ~/.config/zero-g
cat > ~/.config/zero-g/agy-integration.env <<'EOF'
ZERO_G_URL=https://zero-g.example.com
ZERO_G_VIRTUAL_KEY=<virtual key>
ZERO_G_AGY_ACCOUNT=<credential name>
EOF
chmod 600 ~/.config/zero-g/agy-integration.env
```

The virtual key must have no provider allowlist. Model restrictions on the key still apply, and keys limited to a provider are refused. `ZERO_G_AGY_ACCOUNT` is only used by `quota`. `zero-g-agy-integration --help` lists every setting.

## Quota

`agy --print /usage --output-format json` prints each model pool's 5-hour and weekly limits. It uses no tokens and runs with the login `agy` already has. `zero-g-agy-integration quota` runs that command and posts the result to `POST /v1/agy/quota`.

### Setup

1. In the dashboard, open **Models → Providers → Google Antigravity** and add one credential per Google account. Name it after the account. There is no sign-in: the card shows as connected once the first report arrives.
2. On each machine where `agy` is signed in, write the settings file above and set `ZERO_G_AGY_ACCOUNT` to that credential name.
3. Start the background service. It reports once at start and then every 15 minutes. `brew upgrade` keeps it current.

   ```bash
   brew services start zero-g-agy-integration
   ```

4. To check it by hand, run `zero-g-agy-integration quota` once. It prints nothing on success and the reason on failure. Service logs are in `$(brew --prefix)/var/log/`.

Without Homebrew, run `zero-g-agy-integration quota` from cron or a systemd timer.

### Behavior

| Topic             | What happens                                                                                                                                                                                                                                                                                                                                                            |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Matching          | Zero-G finds the credential by name, ignoring case and surrounding spaces. An unknown name gets a 404.                                                                                                                                                                                                                                                                  |
| Freshness         | The card shows the last report. If none arrives for 3 hours, it shows an error naming the service instead of old numbers.                                                                                                                                                                                                                                               |
| Reset times       | A pool that has not started a window reports no reset time, so the card shows none.                                                                                                                                                                                                                                                                                     |
| Subscription plan | The card shows the plan (for example `Google AI Pro`) next to the name. `agy` reports it only through its ACP server, and each read leaves a throwaway conversation file of about 400 KB in its storage. The program reads it at most once a day (`AGY_TIER_REFRESH_HOURS`) and resends the saved value. If the read fails, the quota is still pushed without the plan. |
| Errors            | The command exits non-zero and prints the reason when `agy` output is unexpected or Zero-G refuses the report.                                                                                                                                                                                                                                                          |

## Observation hook

The `PostInvocation` hook sends a record of each interactive `agy` model response to Zero-G, so the activity shows up in the request logs. It is off until you add it.

The hook sends prompts and responses to Zero-G, which stores them. They can contain secrets or private code, so enable it only for a gateway and virtual key you trust. Google's terms do not say whether exporting transcripts like this is allowed. Read them for your account first.

### Setup

1. Write the settings file above. The hook needs only `ZERO_G_URL` and `ZERO_G_VIRTUAL_KEY`.
2. Merge this entry into `.agents/hooks.json` for one workspace, or into `~/.gemini/config/hooks.json` for all workspaces. Other Antigravity products share that second file. Keep any existing hooks. If `zero-g-agy-integration` is not on the `PATH` that `agy` runs with, use its full path, for example `$(brew --prefix)/bin/zero-g-agy-integration`.

   ```json
   {
     "zero-g-observations": {
       "PostInvocation": [
         {
           "type": "command",
           "command": "zero-g-agy-integration observe",
           "timeout": 5
         }
       ]
     }
   }
   ```

3. Run `/hooks` in `agy` to confirm it loaded (see the [official hooks documentation](https://antigravity.google/docs/hooks/)). To stop collecting, remove or disable the hook.

### What it sends

| Item     | Details                                                                                                                                          |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| Endpoint | `POST /v1/agy/observations`, authenticated with the virtual key                                                                                  |
| Record   | The first completed model response in the transcript at or after `initialNumSteps`. It is not a full wire-level request log.                     |
| Request  | Only the hook's `lastUserInput`                                                                                                                  |
| Response | The transcript record, which can include thinking, tool arguments, cached-token counts and truncated content                                     |
| Tokens   | Missing counts are sent as null and never estimated. `input_tokens` excludes cached input. `cache_read_tokens`, if present, stays in the record. |
| Latency  | No duration is measured, so the dashboard shows 0 ms                                                                                             |

The hook reads only transcripts inside `~/.gemini/antigravity-cli/brain`. It skips symlinks and anything else, and it searches the last 2 MiB of the file.

### Limits

- The hook always prints `{}` and exits 0, so it never interrupts `agy`. Missing settings, malformed events, missing or incomplete records, transcript format changes, a response outside the last 2 MiB, network errors and timeouts each cause the record to be skipped silently. The whole run is capped at 4 seconds.
- It has been tested against real `agy` transcripts, but not yet run by a live `agy` session.

See [Gateways](gateways.md) for the endpoints Zero-G serves and [Configuration](configuration.md) for providers and virtual keys.
