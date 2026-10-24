# Development Guide

## Prerequisites

- Rust stable toolchain 1.80+ (`rustc`, `cargo`, `rustfmt`, `clippy`)
- `just` task runner

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
