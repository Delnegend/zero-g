#!/usr/bin/env bash
set -euo pipefail

(cd frontend && bun install --frozen-lockfile)
just frontend-build
