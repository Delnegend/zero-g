default:
    @just --list

# Run the Rust backend and Vite together
dev: frontend-build
    bun run --parallel 'just dev-backend' 'just dev-frontend'

dev-backend:
    ZERO_G_DASHBOARD_ORIGIN=http://127.0.0.1:5173 cargo run --bin zero-g

dev-frontend:
    cd frontend && bun run --bun vite

# Add example providers, keys and logs to the initialized development database (server stopped)
seed:
    bun run frontend/scripts/seed.ts

frontend-build:
    cd frontend && bun run tsc -b && bun run --bun vite build

lint:
    cd frontend && bun run oxlint src scripts

# Regenerate the consolidated OpenAPI and dashboard client contracts
api-generate:
    ZERO_G_OPENAPI_OUTPUT="$PWD/frontend/src/server/api/generated/openapi.json" cargo test --quiet --bin zero-g contract::tests::settings_contract_exposes_operations_and_u32_bounds -- --exact
    bun fmt --no-script frontend/src/server/api/generated/openapi.json
    cd frontend && bun run orval --config orval.config.ts
    bun fmt --no-script frontend/src/server/api/generated

# Detect stale generated contracts without changing project files
api-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    mkdir -p "$tmp/frontend/src/server/api/generated"
    cp frontend/orval.config.ts "$tmp/frontend/"
    cp frontend/src/server/api/client.ts "$tmp/frontend/src/server/api/"
    ln -s "$PWD/frontend/node_modules" "$tmp/frontend/node_modules"
    ZERO_G_OPENAPI_OUTPUT="$tmp/frontend/src/server/api/generated/openapi.json" cargo test --quiet --bin zero-g contract::tests::settings_contract_exposes_operations_and_u32_bounds -- --exact
    bun fmt --no-script "$tmp/frontend/src/server/api/generated/openapi.json"
    (cd "$tmp/frontend" && bun run orval --config orval.config.ts)
    bun fmt --no-script "$tmp/frontend/src/server/api/generated"
    diff -ru frontend/src/server/api/generated "$tmp/frontend/src/server/api/generated" || { echo "API contracts are stale; run just api-generate" >&2; exit 1; }

# Keep the OSV exception limited to the unused Toasty MySQL dependency.
check-dependencies:
    #!/usr/bin/env bash
    set -euo pipefail
    tree="$(cargo tree --locked --target all --all-features --prefix none --format '{p}')"
    while IFS= read -r package; do
        case "$package" in
            "rsa v0.10.0-rc.19"|"rsa v0.10.0-rc.19 (*)")
                echo "rsa 0.10.0-rc.19 is now used; remove the OSV exception and reassess it" >&2
                exit 1
                ;;
        esac
    done <<< "$tree"

# Single gate CI verification
check:
    just check-dependencies
    bun fmt --check --no-script frontend
    just lint
    just frontend-build
    just api-check
    cd frontend && bun test src/server/api/settings.test.ts src/RequestBody.test.tsx
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Format all code
fmt:
    cargo fmt
    bun fmt --no-script frontend

# Run tests
test: frontend-build
    cargo test

# Build release binary
build: frontend-build
    cargo build --release


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
