default:
    @just --list

# Run the Rust backend and Vite together
dev: web-build
    bun run --parallel 'just dev-backend' 'just dev-frontend'

dev-backend:
    ZERO_G_DASHBOARD_ORIGIN=http://127.0.0.1:5173 cargo run --bin zero-g -- serve

dev-frontend:
    cd web && bun run --bun vite

web-build:
    cd web && bun run tsc -b && bun run --bun vite build

lint:
    cd web && bun run oxlint src

# Single gate CI verification
check:
    bun fmt --check --no-script web assets/zen_shim.ts
    just lint
    just web-build
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Format all code
fmt:
    cargo fmt
    bun fmt --no-script web assets/zen_shim.ts

# Run tests
test: web-build
    cargo test

# Build release binary
build: web-build
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
