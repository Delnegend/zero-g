# Development Guide

## Prerequisites

- Rust stable toolchain 1.95+ (`rustc`, `cargo`, `rustfmt`, `clippy`)
- `just` task runner
- Bun 1.4.4

Install frontend dependencies with `(cd frontend && bun install --frozen-lockfile)`. Build the frontend with `just frontend-build` before invoking `cargo build` directly; its files are embedded into the binary. `just build`, `just test`, and `just check` build it automatically.

For live frontend reload, start the Rust backend and Vite together:

```bash
just dev
```

The recipe builds the frontend once for the Rust binary, then runs both servers in parallel. Press Ctrl+C to stop them.

Open `http://127.0.0.1:5173`. Vite forwards `/api`, `/auth/callback` and `/redoc` to the Rust backend at `127.0.0.1:8080`; it preserves the browser's host and origin for dashboard authentication. Vite does not proxy the unified `/v1` inference API: point API clients to `http://127.0.0.1:8080/v1`. Configure Basic/OIDC and the encryption key as described in the [configuration reference](configuration.md). For OIDC, register and set `ZERO_G_OIDC_REDIRECT_URL=http://127.0.0.1:5173/auth/callback` for this local setup. Vite's port is fixed; if 5173 is in use, stop the other process rather than silently switching origins.
Vite checks dashboard access with the Rust backend before serving the page or frontend assets. In Basic mode, open the page in a browser that supports the HTTP Basic sign-in prompt. If the backend is unavailable, Vite returns 502 instead of serving the dashboard.
Use the exact `127.0.0.1` address above: `localhost` is a different origin and is rejected with `Invalid request origin`.

Run `just fmt` to format Rust, frontend, and shim code, and `just lint` to lint the frontend.
Dashboard pages use the shared skeleton components in `frontend/src/components/Loading.tsx` for initial data loads. The dashboard keeps the previous results visible and shows **Updating** while a refresh or new time range loads, preventing layout shifts.
Dashboard and catalog refresh controls share `frontend/src/components/RefreshButton.tsx`, including their disabled and updating states.
Dashboard page titles are defined in their TanStack Router route metadata in `frontend/src/App.tsx`.
The requests chart uses TanStack Charts' motion renderer and theme-aware tooltip tokens; motion respects the browser's reduced-motion preference.
The request log table uses TanStack Table v9 with explicit filtering, sorting and pagination features.

To populate the dashboard, start zero-g once to initialize its database, then stop it and run `just seed`. It reads `ZERO_G_LOG_DB` from `.env` (or uses `zero-g-logs.db`), creates example providers where needed, and appends 140 example requests and two restricted virtual keys per run. It reuses an existing provider of each kind without changing its credentials. Request and response bodies contain `_seed: true`. Copy the newly generated keys from the command output; they cannot be retrieved later. Newly created providers require upstream credentials before making requests. If an example ID belongs to a different or deleted provider, the seed fails without adding data. Use a development database, not production. Restart `just dev` to view the data.

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

Rust follows the [Namtao strict Clippy configuration](https://www.namtao.com/rust/#strict-lints): pedantic and nursery lints, plus explicit panic, indexing, arithmetic, conversion, and process-exit checks. `clippy.toml` permits the guide's test-only exceptions. Visibility follows actual usage: keep items private when possible and expose them only to the modules that need them. The `redundant_pub_crate` lint is disabled because it recommends broader visibility in binary crates; individual exceptions document cohesive long functions or JSON object insertion.

Clippy also requires documentation comments on private Rust items, including fields and variants. Describe their purpose or constraints with `///`; `just check` treats missing documentation as an error.

## API contract

Rust handlers and their request and response types define the OpenAPI document. After changing a dashboard or inference contract, run `just api-generate` and commit the generated files in `frontend/src/server/api/generated/`. Orval generates the dashboard request functions and Zod response validators from that document; `just check` detects stale generated files and runs the frontend client tests.

Shared TanStack Query options and cache keys live with the corresponding API functions in `frontend/src/server/api/`. Components call `useQuery` with those options and reuse their keys when updating or invalidating cached data.

## Database migrations

The models in `backend/storage.rs` define the Turso schema. After changing them, generate a migration with `cargo run --bin migrate -- migration generate --name describe_change`; commit `toasty/history.toml`, the generated migration and its snapshot together. The server embeds and applies tracked migrations on startup. Do not use `push_schema` on production databases. Stop writers and back up the database before upgrading.

## Release Workflow

Releases are handled autonomously via [Delnegend/actions](https://github.com/Delnegend/actions). Version bumps follow the Conventional Commits contract:

Run `just version get` to inspect the current version. To change it without committing, run `just version bump X.Y.Z`, replacing `X.Y.Z` with the intended version.

## Contribution Workflow

1. Create a feature branch: `git checkout -b feat/my-change`.
2. Implement changes and ensure `just check` passes cleanly.
3. Open a Pull Request with a clear summary of user outcomes.
