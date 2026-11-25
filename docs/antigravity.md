# Antigravity

Zero-G does not route Antigravity requests, because Google's terms of service forbid it. The best it can do is track your usage. This page covers the two optional tools that do that.

## What Zero-G does

Your `agy` CLI talks to Google directly, and Zero-G never sees your Google login.

Two small tools run on the machine where you use `agy`. They read what the official CLI already knows and report it to Zero-G:

- The quota collector reports how much of your 5-hour and weekly limits is left and which plan you are on. It appears on the Google Antigravity credential card.
- The observation hook records each model response in the request logs.

Use the collector if you only want to see your limits. Add the hook if you also want prompts and responses in the logs.

A Google Antigravity provider in the dashboard is only a place to show quota. It has no model routes and does not appear in `/v1/models`.

## Shared setup

Both tools need a virtual key that has no provider allowlist. Model restrictions on the key still apply, and keys limited to a provider are refused. Set these variables where the tool runs, using HTTPS when the gateway is remote:

```bash
export ZERO_G_URL=http://127.0.0.1:8080  # gateway root, without /v1
export ZERO_G_VIRTUAL_KEY='<virtual key>'
```

## Quota collector

`agy --print /usage --output-format json` prints each model pool's 5-hour and weekly limits. It uses no tokens and runs with the login `agy` already has. The `zero-g-agy-quota` collector runs that command and posts the result to `POST /v1/agy/quota`.

The collector is released with Zero-G and has the same version number. Each [GitHub release](https://github.com/Delnegend/zero-g/releases) includes static binaries for Linux on x86-64 and arm64. Homebrew installs and updates them.

### Setup

1. In the dashboard, open **Models → Providers → Google Antigravity** and add one credential per Google account. Name it after the account. There is no sign-in: the card shows as connected once the first report arrives.
2. On each machine where `agy` is signed in, install the collector and create its settings file:

   ```bash
   brew tap Delnegend/tap
   brew install zero-g-agy-quota

   mkdir -p ~/.config/zero-g
   cat > ~/.config/zero-g/agy-quota.env <<'EOF'
   ZERO_G_URL=https://zero-g.example.com
   ZERO_G_VIRTUAL_KEY=<virtual key>
   ZERO_G_AGY_ACCOUNT=<credential name>
   EOF
   chmod 600 ~/.config/zero-g/agy-quota.env
   ```

3. Start it as a background service. It reports once at start and then every 15 minutes. `brew upgrade` keeps it current.

   ```bash
   brew services start zero-g-agy-quota
   ```

4. To check it by hand, run `zero-g-agy-quota` once. It prints nothing on success and the reason on failure. Service logs are in `$(brew --prefix)/var/log/`.

Without Homebrew, download the archive for your platform from the release page and run `zero-g-agy-quota` from cron or a systemd timer. Settings come from the environment or the settings file. Environment variables win, and `--config FILE` points to a different file. `zero-g-agy-quota --help` lists every setting.

### Behavior

| Topic             | What happens                                                                                                                                                                                                                                                                                                                                                              |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Matching          | Zero-G finds the credential by name, ignoring case and surrounding spaces. An unknown name gets a 404.                                                                                                                                                                                                                                                                    |
| Freshness         | The card shows the last report. If none arrives for 3 hours, it shows an error naming the collector instead of old numbers.                                                                                                                                                                                                                                               |
| Reset times       | A pool that has not started a window reports no reset time, so the card shows none.                                                                                                                                                                                                                                                                                       |
| Subscription plan | The card shows the plan (for example `Google AI Pro`) next to the name. `agy` reports it only through its ACP server, and each read leaves a throwaway conversation file of about 400 KB in its storage. The collector reads it at most once a day (`AGY_TIER_REFRESH_HOURS`) and resends the saved value. If the read fails, the quota is still pushed without the plan. |
| Errors            | The collector exits non-zero and prints the reason when `agy` output is unexpected or Zero-G refuses the report.                                                                                                                                                                                                                                                          |

## Observation hook

The `PostInvocation` hook sends a record of each interactive `agy` model response to Zero-G, so the activity shows up in the request logs. It is off until you add it.

The hook sends prompts and responses to Zero-G, which stores them. They can contain secrets or private code, so enable it only for a gateway and virtual key you trust. Google's terms do not say whether exporting transcripts like this is allowed. Read them for your account first.

### Setup

1. Export the shared variables in the environment that launches `agy`. The hook needs Python 3.
2. Merge this entry into `.agents/hooks.json` for one workspace, or into `~/.gemini/config/hooks.json` for all workspaces. Other Antigravity products share that second file. Use the absolute path to your checkout and keep any existing hooks.

   ```json
   {
     "zero-g-observations": {
       "PostInvocation": [
         {
           "type": "command",
           "command": "python3 /absolute/path/to/zero-g/scripts/agy-observe.py",
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

### Limits

- Collection is best effort and never interrupts `agy`. Missing or incomplete records, transcript format changes, a response outside the final 2 MiB, network errors and timeouts can each cause a record to be skipped.
- The hook has not been tested against a live `agy` session.

See [Gateways](gateways.md) for the endpoints Zero-G serves and [Configuration](configuration.md) for providers and virtual keys.
