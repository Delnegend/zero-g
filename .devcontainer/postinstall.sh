#!/usr/bin/env bash
set -e
# Rust toolchain (stable), musl target, mold, and flamegraph are pre-installed in
# the Dockerfile for cache + Zed (postCreateCommand is skipped in some editors).
# This is a pure-Rust CLI with no frontend deps, so there is nothing to install
# here — the hook is kept as the place to warm the cargo cache or add
# project-specific tooling later.
