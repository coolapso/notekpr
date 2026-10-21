# syntax=docker/dockerfile:1.7
# coolpsonotes server image: the zero-knowledge sync server plus the browser
# client (Rust → WebAssembly). Built with the Rust toolchain only — no npm.
#
# Multi-platform: `docker buildx build --platform linux/amd64,linux/arm64`.
# The build stages run on the build machine's own architecture and
# cross-compile; the final stages only copy files, so nothing is emulated.
# Cache ids are per stage and architecture: two cargo processes must not
# share a registry or target directory at the same time.

# The browser client. WebAssembly is the same on every architecture, so a
# multi-platform build makes it once.
FROM --platform=$BUILDPLATFORM rust:1.99-slim-trixie AS web
ARG WASM_BINDGEN_VERSION=0.2.128
RUN rustup target add wasm32-unknown-unknown
# wasm-opt makes the browser bundle much smaller.
RUN apt-get update && apt-get install -y --no-install-recommends binaryen && rm -rf /var/lib/apt/lists/*
RUN --mount=type=cache,id=coolpsonotes-registry-web,target=/usr/local/cargo/registry \
    cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_VERSION} --locked
WORKDIR /src
COPY . .
RUN --mount=type=cache,id=coolpsonotes-registry-web,target=/usr/local/cargo/registry \
    --mount=type=cache,id=coolpsonotes-target-web,target=/src/target \
    ./scripts/build-web.sh \
 && mkdir -p /out \
 && cp -r dist/web /out/web

# The server and the CLI for the image's architecture (TARGETARCH). For
# another architecture than the build machine's, Debian's cross C compiler
# links them and builds their C parts (SQLite, ring, aws-lc). Debian's own
# gcc answers to the same <arch>-linux-gnu-gcc name, so one set of settings
# serves native and cross builds.
FROM --platform=$BUILDPLATFORM rust:1.99-slim-trixie AS build
ARG BUILDARCH
ARG TARGETARCH
RUN case "$TARGETARCH" in \
      amd64) arch=x86_64 gcc=gcc-x86-64-linux-gnu libc=libc6-dev-amd64-cross ;; \
      arm64) arch=aarch64 gcc=gcc-aarch64-linux-gnu libc=libc6-dev-arm64-cross ;; \
      *) echo "no Rust target set up for $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && echo "$arch-unknown-linux-gnu" > /rust-target \
 && rustup target add "$arch-unknown-linux-gnu" \
 && if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update && apt-get install -y --no-install-recommends "$gcc" "$libc" \
      && rm -rf /var/lib/apt/lists/*; \
    fi
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
WORKDIR /src
COPY . .
RUN --mount=type=cache,id=coolpsonotes-registry-$TARGETARCH,target=/usr/local/cargo/registry \
    --mount=type=cache,id=coolpsonotes-target-$TARGETARCH,target=/src/target \
    target=$(cat /rust-target) \
 && cargo build --release --locked --target "$target" -p coolpsonotes-server -p coolpsonotes-client \
 && mkdir -p /out/bin /out/data \
 && cp "target/$target/release/coolpsonotes-server" "target/$target/release/coolpsonotes" /out/bin/

# A public demo that starts over every hour (docs/operations.md §10): the
# server and CLI, plus Python for scripts/demo.py, which seeds the demo data
# through the CLI, serves it for an hour and exits so it is restarted fresh.
# Development mode: anyone who reaches it can sign in as anyone. Built only
# on request (`task docker:build:demo`, i.e. --target demo); the default
# target is the server image below.
FROM python:3.13-slim-trixie AS demo
COPY --from=build /out/bin/ /usr/local/bin/
COPY --from=web /out/web /usr/share/coolpsonotes/web
COPY scripts/demo.py /usr/share/coolpsonotes/demo.py
COPY --from=build --chown=65532:65532 /out/data /data
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["python3", "-u", "/usr/share/coolpsonotes/demo.py", \
    "--server-bin", "/usr/local/bin/coolpsonotes-server", "--cli-bin", "/usr/local/bin/coolpsonotes", \
    "--web-dir", "/usr/share/coolpsonotes/web", "--data", "/data/demo", "--server-log", "-", \
    "--listen", "0.0.0.0:8080", "--dev-mode-anywhere", "--fresh", "--lifetime", "1h"]

FROM gcr.io/distroless/cc-debian13:nonroot
LABEL org.opencontainers.image.title="coolpsonotes" \
      org.opencontainers.image.description="Self-hosted, end-to-end encrypted, collaborative Markdown notes" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /out/bin/ /usr/local/bin/
COPY --from=web /out/web /usr/share/coolpsonotes/web
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
