# Development Guide

## Prerequisites

- Rust stable toolchain 1.95+ (`rustc`, `cargo`, `rustfmt`, `clippy`)
- `just` task runner
- Bun 1.4.4

Install frontend dependencies with `(cd web && bun install --frozen-lockfile)`. Build the frontend with `just web-build` before invoking `cargo build` directly; its files are embedded into the binary. `just build`, `just test`, and `just check` build it automatically.

For live frontend reload, start the Rust backend and Vite together:

```bash
just dev
```

The recipe builds the frontend once for the Rust binary, then runs both servers in parallel. Press Ctrl+C to stop them.

Open `http://127.0.0.1:5173`. Vite forwards `/api` and `/auth/callback` to the Rust backend at `127.0.0.1:8080`; it preserves the browser's host and origin for dashboard authentication. Configure Basic/OIDC and the encryption key as described in the [configuration reference](configuration.md). For OIDC, register and set `ZERO_G_OIDC_REDIRECT_URL=http://127.0.0.1:5173/auth/callback` for this local setup. Vite's port is fixed; if 5173 is in use, stop the other process rather than silently switching origins.
Vite checks dashboard access with the Rust backend before serving the page or frontend assets. In Basic mode, open the page in a browser that supports the HTTP Basic sign-in prompt. If the backend is unavailable, Vite returns 502 instead of serving the dashboard.
Use the exact `127.0.0.1` address above: `localhost` is a different origin and is rejected with `Invalid request origin`.

Run `just fmt` and `just lint` from the repository root to format and lint the frontend.

To populate the dashboard with example request logs, start zero-g once to initialize its database, then stop it and run `just seed`. It reads `ZERO_G_LOG_DB` from `.env` (or uses `zero-g-logs.db`) and appends 140 example requests spanning the past six days. Each run adds another 140; request and response bodies contain `_seed: true`. Use a development database, not production. Restart `just dev` to view the data.

## Build & Test

All validation runs through the project's gate command:

```bash
# Verify format, lints, and unit tests (single CI gate)
just check

# Run unit tests only
just test

# Format code
just fmt

# Compile release binary
just build
```

## Settings API contract

The Rust settings request and response types define the settings API. After changing them, run `just api-generate` and commit `web/src/api/generated/openapi.json` and `web/src/api/generated/settings.ts`; Orval generates the Zod validators used by the settings client in `web/src/api/settings.ts`. TanStack Query manages the dashboard's server state. `just check` verifies that both generated files match the Rust contract and runs the frontend client tests.

This is a pilot for `/api/settings`; the other dashboard endpoints still use their existing handwritten types and fetch calls.

## Database migrations

The models in `src/storage.rs` define the Turso schema. After changing them, generate a migration with `cargo run --bin migrate -- migration generate --name describe_change`; commit `toasty/history.toml`, the generated migration and its snapshot together. The server embeds and applies tracked migrations on startup. Do not use `push_schema` on production databases. Stop writers and back up the database before upgrading. Legacy libSQL databases require the separate import procedure in the [deployment guide](deployment.md#upgrade-migration-and-backup).

## Release Workflow

Releases are handled autonomously via [Delnegend/actions](https://github.com/Delnegend/actions). Version bumps follow the Conventional Commits contract:

```bash
# Inspect current version
just version get

# Bump version in manifest without committing
just version bump 0.2.0
```

## Contribution Workflow

1. Create a feature branch: `git checkout -b feat/my-change`.
2. Implement changes and ensure `just check` passes cleanly.
3. Open a Pull Request with a clear summary of user outcomes.
