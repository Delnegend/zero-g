default:
    @just --list

# Run the Rust backend and Vite together
dev: frontend-build
    bun run --parallel 'just dev-backend' 'just dev-frontend'

dev-backend:
    ZERO_G_DASHBOARD_ORIGIN=http://127.0.0.1:5173 cargo run --bin zero-g

dev-frontend:
    cd frontend && bun run --bun vite

# Add example request logs to the initialized development database (server stopped)
seed:
    bun run frontend/scripts/seed.ts

frontend-build:
    cd frontend && bun run tsc -b && bun run --bun vite build

lint:
    cd frontend && bun run oxlint src scripts

# Regenerate the settings API contract and validators
api-generate:
    ZERO_G_OPENAPI_OUTPUT="$PWD/frontend/src/api/generated/openapi.json" cargo test --quiet --bin zero-g contract::tests::settings_contract_exposes_operations_and_u32_bounds -- --exact
    bun fmt --no-script frontend/src/api/generated/openapi.json
    cd frontend && bun run orval --config orval.config.ts
    bun fmt --no-script frontend/src/api/generated/settings.ts

# Detect stale generated settings contracts without changing project files
api-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    ZERO_G_OPENAPI_OUTPUT="$tmp/openapi.json" cargo test --quiet --bin zero-g contract::tests::settings_contract_exposes_operations_and_u32_bounds -- --exact
    bun fmt --no-script "$tmp/openapi.json"
    cmp "$tmp/openapi.json" frontend/src/api/generated/openapi.json || { echo "OpenAPI is stale; run just api-generate" >&2; exit 1; }
    (cd frontend && ZERO_G_OPENAPI_INPUT="$tmp/openapi.json" ZERO_G_CONTRACT_OUTPUT="$tmp/settings.ts" bun run orval --config orval.config.ts)
    bun fmt --no-script "$tmp/settings.ts"
    cmp "$tmp/settings.ts" frontend/src/api/generated/settings.ts || { echo "Zod validators are stale; run just api-generate" >&2; exit 1; }

# Single gate CI verification
check:
    bun fmt --check --no-script frontend assets/zen_shim.ts
    just lint
    just frontend-build
    just api-check
    cd frontend && bun test src/api/settings.test.ts src/RequestBody.test.tsx
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Format all code
fmt:
    cargo fmt
    bun fmt --no-script frontend assets/zen_shim.ts

# Run tests
test: frontend-build
    cargo test

# Build release binary
build: frontend-build
    cargo build --release

# Compile the Zen TLS shim to a standalone binary (pinned Bun: the gate
# fingerprint depends on the exact Bun/BoringSSL build)
shim:
    bun build --compile ./assets/zen_shim.ts --outfile ./zen-shim


# Language-agnostic version contract for Delnegend/actions/resolve-release.
# `just version get` prints X.Y.Z; `just version set X.Y.Z` rewrites manifests.
version action ver="":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{action}}" in
      get)
        python3 -c "import tomllib;d=tomllib.load(open('Cargo.toml','rb'));print(d.get('workspace',{}).get('package',{}).get('version') or d.get('package',{}).get('version',''))"
        ;;
      set|bump)
        [ -n "{{ver}}" ] || { echo "usage: just version set X.Y.Z" >&2; exit 1; }
        sed -i -E '0,/^version = ".*"/s//version = "{{ver}}"/' Cargo.toml
        cargo check
        ;;
      *) echo "usage: just version get|set X.Y.Z" >&2; exit 1 ;;
    esac
