#!/usr/bin/env bash
set -euo pipefail

# Install Bun if not already available
if ! command -v bun >/dev/null 2>&1; then
    curl -fsSL https://bun.sh/install | bash
    export BUN_INSTALL="${HOME}/.bun"
    export PATH="${BUN_INSTALL}/bin:${PATH}"
fi

(cd frontend && bun install --frozen-lockfile)
just frontend-build
