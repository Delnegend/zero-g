FROM oven/bun:1.4.4 AS frontend
WORKDIR /app/frontend
COPY frontend ./
RUN bun install --frozen-lockfile && bun run tsc -b && bun run --bun vite build

# Build stage
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev clang-dev cmake make
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
COPY assets ./assets
COPY backend ./backend
COPY Toasty.toml ./
COPY toasty ./toasty
COPY --from=frontend /app/frontend/dist ./frontend/dist
RUN mkdir -p /app/data && chown 65532:65532 /app/data
RUN cargo build --release --locked

# Runtime stage: minimal static Debian 13 distroless
FROM gcr.io/distroless/static-debian13:nonroot
WORKDIR /app
COPY --from=builder /app/target/release/zero-g /usr/local/bin/zero-g
COPY --from=builder --chown=65532:65532 /app/data /app/data

ENV ZERO_G_LOG_DB=/app/data/zero-g-logs.db

EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/zero-g"]
