FROM docker.io/oven/bun:1.4.4 AS frontend
WORKDIR /app/frontend
COPY frontend ./
RUN bun install --frozen-lockfile && bun run tsc -b && bun run --bun vite build

# Build stage
FROM docker.io/library/rust:1-alpine AS builder
RUN apk add --no-cache git musl-dev clang-dev cmake make
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs Toasty.toml ./
COPY agy-integration ./agy-integration
COPY backend ./backend
COPY toasty ./toasty
COPY --from=frontend /app/frontend/dist ./frontend/dist
RUN mkdir -p /app/data && chown 65532:65532 /app/data
# The cache mounts keep downloaded crates and compiled dependencies between
# builds (the release workflow saves them to the GitHub Actions cache), so a
# dependency update recompiles only the crates that changed. The binary is
# copied out because cache mounts are not part of the image.
RUN --mount=type=cache,id=zero-g-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=zero-g-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=zero-g-target,target=/app/target \
    cargo build --release --locked --bin zero-g \
    && cp target/release/zero-g /usr/local/bin/zero-g

# Runtime stage: minimal static Debian 13 distroless
FROM gcr.io/distroless/static-debian13:nonroot
WORKDIR /app
COPY --from=builder /usr/local/bin/zero-g /usr/local/bin/zero-g
COPY --from=builder --chown=65532:65532 /app/data /app/data

ENV ZERO_G_LOG_DB=/app/data/zero-g.db

EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/zero-g"]
