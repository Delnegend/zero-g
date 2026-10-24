# Build stage
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# Runtime stage: minimal static Debian 13 distroless
FROM gcr.io/distroless/static-debian13:nonroot
WORKDIR /app
COPY --from=builder /app/target/release/zero-g /usr/local/bin/zero-g

ENV ZERO_G_BIND=0.0.0.0:8080

EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/zero-g"]
CMD ["serve"]
