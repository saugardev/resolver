FROM rust:1.94.0-bookworm@sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f AS builder

WORKDIR /src
COPY . .
RUN cargo build --locked --release

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241 AS runtime

LABEL org.opencontainers.image.title="Livy Resolver" \
      org.opencontainers.image.source="https://github.com/livylabs/resolver"

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 resolver \
    && useradd --uid 10001 --gid resolver --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin resolver

COPY --from=builder --chown=10001:10001 /src/target/release/livy-resolver /usr/local/bin/livy-resolver

USER 10001:10001
EXPOSE 3001
ENV PORT=3001
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl --fail --silent "http://127.0.0.1:${PORT}/healthz" || exit 1
ENTRYPOINT ["/usr/local/bin/livy-resolver"]
