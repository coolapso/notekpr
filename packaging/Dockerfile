# syntax=docker/dockerfile:1.7
# The machine the CLI's release files are built on (.goreleaser.yaml): Rust
# with every target the CLI is built for, zig and cargo-zigbuild to compile
# and link them all (for an old enough glibc) and GoReleaser. packaging/run.sh
# builds it and runs commands in it with the checkout at /src, so a developer
# and CI need only Docker (`task build:clients`, `task release:clients`).
#
# Upgrading a tool: change its version and checksums together (the
# checksums come from the tool's own release page).
FROM rust:1.99-slim-trixie

ARG TARGETARCH

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl xz-utils git openssh-client \
 && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu

ARG ZIG_VERSION=0.16.0
ARG ZIG_SHA256_AMD64=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00
ARG ZIG_SHA256_ARM64=ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17
ARG GORELEASER_VERSION=2.19.0
ARG GORELEASER_SHA256_AMD64=ebc3194ad1d1b1787614405285a0039665cd4c5bd3305215a1b201600f90630b
ARG GORELEASER_SHA256_ARM64=ef33741b684c9f62d6a21e7a3c8b051f139ea7b1d301693b67cc819c46226d53
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) zig=x86_64 zigsum=$ZIG_SHA256_AMD64 gr=x86_64 grsum=$GORELEASER_SHA256_AMD64 ;; \
      arm64) zig=aarch64 zigsum=$ZIG_SHA256_ARM64 gr=arm64 grsum=$GORELEASER_SHA256_ARM64 ;; \
      *) echo "no tools set up for $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/zig.tar.xz "https://ziglang.org/download/$ZIG_VERSION/zig-$zig-linux-$ZIG_VERSION.tar.xz"; \
    echo "$zigsum  /tmp/zig.tar.xz" | sha256sum -c -; \
    mkdir /opt/zig && tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1; \
    ln -s /opt/zig/zig /usr/local/bin/zig; \
    curl -fsSL -o /tmp/goreleaser.tar.gz "https://github.com/goreleaser/goreleaser/releases/download/v$GORELEASER_VERSION/goreleaser_Linux_$gr.tar.gz"; \
    echo "$grsum  /tmp/goreleaser.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/goreleaser.tar.gz -C /usr/local/bin goreleaser; \
    rm /tmp/zig.tar.xz /tmp/goreleaser.tar.gz

ARG CARGO_ZIGBUILD_VERSION=0.24.0
RUN cargo install cargo-zigbuild --version "$CARGO_ZIGBUILD_VERSION" --locked \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/.package-cache

# Commands run as the developer's own user (packaging/run.sh), so files they
# write in the checkout are theirs: let any user use the toolchain.
RUN chmod -R a+w /usr/local/cargo /usr/local/rustup
