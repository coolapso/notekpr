# syntax=docker/dockerfile:1.7
# coolpsonotes server image: the zero-knowledge sync server plus the browser
# client (Rust → WebAssembly). Built with the Rust toolchain only — no npm.

FROM rust:1.99-slim-trixie AS build
ARG WASM_BINDGEN_VERSION=0.2.128
RUN rustup target add wasm32-unknown-unknown
# wasm-opt makes the browser bundle much smaller.
RUN apt-get update && apt-get install -y --no-install-recommends binaryen && rm -rf /var/lib/apt/lists/*
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_VERSION} --locked
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p coolpsonotes-server -p coolpsonotes-client \
 && ./scripts/build-web.sh \
 && mkdir -p /out/bin /out/data \
 && cp target/release/coolpsonotes-server target/release/coolpsonotes /out/bin/ \
 && cp -r dist/web /out/web

FROM gcr.io/distroless/cc-debian13:nonroot
LABEL org.opencontainers.image.title="coolpsonotes" \
      org.opencontainers.image.description="Self-hosted, end-to-end encrypted, collaborative Markdown notes" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /out/bin/ /usr/local/bin/
COPY --from=build /out/web /usr/share/coolpsonotes/web
COPY --from=build --chown=65532:65532 /out/data /data
ENV COOLPSONOTES_LISTEN=0.0.0.0:8080 \
    COOLPSONOTES_DATA_DIR=/data \
    COOLPSONOTES_WEB_DIR=/usr/share/coolpsonotes/web \
    COOLPSONOTES_LOG_FORMAT=json
VOLUME ["/data"]
EXPOSE 8080
USER 65532:65532
# Checked every second while starting, so a new container counts as healthy
# (and proxies that wait for that, like Traefik's Docker provider, route to
# it) within a second of an update, not after the first 30 s interval.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --start-interval=1s CMD ["coolpsonotes-server", "healthcheck"]
ENTRYPOINT ["coolpsonotes-server"]
CMD ["serve"]
