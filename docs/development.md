# Development Guide

## Prerequisites

- Rust stable toolchain 1.95+ (`rustc`, `cargo`, `rustfmt`, `clippy`)
- `just` task runner
- Bun 1.4.4

Install frontend dependencies with `(cd frontend && bun install --frozen-lockfile)`. Build the frontend with `just frontend-build` before invoking `cargo build` directly; its files are embedded into the binary. `just build`, `just test`, and `just check` build it automatically.

The Antigravity OAuth client values in `backend/provider/oauth-clients.toml` are base64-encoded and decoded during the build. They are shared public-client values; base64 is not encryption and does not make them secret.

Frontend dependency updates use Bun's 14-day minimum release age configured in `frontend/bunfig.toml`; the dependency workflow runs them from `frontend`.

The development container provides Bun 1.4.4 from the official Bun image. Its post-install hook installs the locked frontend dependencies and builds the frontend; it does not download or execute an installer script.

For live frontend reload, start the Rust backend and Vite together:

```bash
just dev
```

The recipe builds the frontend once for the Rust binary, then runs both servers in parallel. Press Ctrl+C to stop them.

Open `http://127.0.0.1:5173`. Vite forwards `/api`, `/auth/callback` and `/redoc` to the Rust backend at `127.0.0.1:8080`; it preserves the browser's host and origin for dashboard authentication. Vite does not proxy the unified `/v1` inference API: point API clients to `http://127.0.0.1:8080/v1`. Configure Basic/OIDC and the encryption key as described in the [configuration reference](configuration.md). For OIDC, register and set `ZERO_G_OIDC_REDIRECT_URL=http://127.0.0.1:5173/auth/callback` for this local setup. Vite's port is fixed; if 5173 is in use, stop the other process rather than silently switching origins.
Vite checks dashboard access with the Rust backend before serving the page or frontend assets. In Basic mode, open the page in a browser that supports the HTTP Basic sign-in prompt. If the backend is unavailable, Vite returns 502 instead of serving the dashboard.
Use the exact `127.0.0.1` address above: `localhost` is a different origin and is rejected with `Invalid request origin`.

Run `just fmt` to format Rust and frontend code, and `just lint` to lint the frontend.
Vite runs the experimental Oxc React Compiler for development and production builds, automatically memoizing eligible components.
Dashboard pages use layout-specific skeleton components for initial data loads. The dashboard keeps the previous results visible and shows **Updating** while a refresh, filter or new time range loads.
Dashboard and catalog refresh controls share `frontend/src/components/RefreshButton.tsx`, including their disabled and updating states.
The root tooltip provider opens shadcn tooltips without delay. Model descriptions show a tooltip only when clipped to three lines; long tooltip text scrolls within the popup.
Dashboard routes, page layouts and titles live in `frontend/src/routes/`, mirroring their URLs. Shared components remain outside the routes directory. The TanStack Router Vite plugin generates `frontend/src/routeTree.gen.ts` during development and builds; commit the generated file when changing routes and do not edit it manually.
Provider management keeps its page in `frontend/src/routes/models/providers.tsx` and its form, credential list and provider card components in `frontend/src/components/providers/`. The add-provider dialog keeps its initial kind choices until it closes, so a refresh after saving cannot replace the selected label.
The dashboard graphs use TanStack Charts' motion renderer and theme-aware tooltip tokens; motion respects the browser's reduced-motion preference.
The request log table uses TanStack Table v9 with explicit filtering, sorting and pagination features.

To populate the dashboard, start zero-g once to initialize its database, then stop it and run `just seed`. It reads `ZERO_G_LOG_DB` from `.env` (or uses `zero-g-logs.db`), creates example providers where needed, and appends 140 example requests and two restricted virtual keys per run. It reuses an existing provider of each kind without changing its credentials. Request and response bodies contain `_seed: true`. Copy the newly generated keys from the command output; they cannot be retrieved later. Newly created providers require upstream credentials before making requests. If an example ID belongs to a different or deleted provider, the seed fails without adding data. Use a development database, not production. Restart `just dev` to view the data.

Seeded logs include example virtual-key identities and synthetic provider-credential identities for testing the dashboard filters. These credential identities do not create or connect provider credentials.

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

Both `just test` and `just check` discover frontend tests under `frontend/src/` and run the Rust test suite.

`just check-dependencies` ensures the OSV exception for `rsa 0.10.0-rc.19` remains limited to Toasty's unused MySQL driver. If that version enters the build, remove the exception in `osv-scanner.toml` and reassess its security advisory.

`osv-scanner.toml` contains temporary, version-specific OIDC and shadcn tooling dependency exceptions through 2026-11-28. Each suppresses all OSV findings for that exact package version, not only the named advisory; review raw findings and dependency usage before renewing. `deepmerge` is used by shadcn's development tool and is not included in the built dashboard.

OSV advisory `GHSA-g3hv-75wg-m249` is ignored until 2026-11-14: the current `minimatch` 5.x version lacks the affected preprocessing, and the 10.x caller uses optimization level 1, not the affected level 2. This exception covers that advisory for all versions, so recheck callers and remove it when a compatible fix is available.

Rust follows the [Namtao strict Clippy configuration](https://www.namtao.com/rust/#strict-lints): pedantic and nursery lints, plus explicit panic, indexing, arithmetic, conversion, and process-exit checks. `clippy.toml` permits the guide's test-only exceptions. Visibility follows actual usage: keep items private when possible and expose them only to the modules that need them. The `redundant_pub_crate` lint is disabled because it recommends broader visibility in binary crates; individual exceptions document cohesive long functions or JSON object insertion.

Clippy also requires documentation comments on private Rust items, including fields and variants. Describe their purpose or constraints with `///`; `just check` treats missing documentation as an error.

## API contract

Rust handlers and their request and response types define the OpenAPI document. After changing a dashboard or inference contract, run `just api-generate` and commit the generated files in `frontend/src/server/api/generated/`. Orval generates the dashboard request functions and Zod response validators from that document; `just check` detects stale generated files and runs the frontend client tests.

The recipes export the contract by starting `zero-g` with `ZERO_G_OPENAPI_OUTPUT` set to an output path. In this mode the binary writes the document and exits without opening the database or starting the server.

Shared TanStack Query options and cache keys live with the corresponding API functions in `frontend/src/server/api/`. Components call `useQuery` with those options and reuse their keys when updating or invalidating cached data.

## Database migrations

The models in `backend/storage.rs` define the Turso schema. After changing them, generate a migration with `cargo run --bin migrate -- migration generate --name describe_change`; commit `toasty/history.toml`, the generated migration and its snapshot together. The server embeds and applies tracked migrations on startup. Do not use `push_schema` on production databases. Stop writers and back up the database before upgrading.

## Release Workflow

The release workflow uses [Delnegend/actions](https://github.com/Delnegend/actions) for versioning and publication. It builds container images with pinned Docker actions so rebuilding an older tag cannot replace `latest`, and labels the image with the commit actually checked out. Version bumps follow the Conventional Commits contract:

Run `just version get` to inspect the current version. To change it without committing, run `just version bump X.Y.Z`, replacing `X.Y.Z` with the intended version.

## Contribution Workflow

1. Create a feature branch: `git checkout -b feat/my-change`.
2. Implement changes and ensure `just check` passes cleanly.
3. Open a Pull Request with a clear summary of user outcomes.
