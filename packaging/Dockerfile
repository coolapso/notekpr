# syntax=docker/dockerfile:1.7
# The machine the CLI's and the desktop app's release files are built on
# (.goreleaser.yaml). packaging/run.sh builds it and runs commands in it with
# the checkout at /src, so a developer and CI need only Docker
# (`task build:clients`, `task release:clients`).
#
# Ubuntu 22.04, the oldest system the desktop app's packages are for: the app
# links against the system's WebKitGTK and GTK, so it is built against the
# oldest ones it runs with (glibc 2.35, WebKitGTK 4.1). The CLI links nothing
# but glibc: zig and cargo-zigbuild build it for glibc 2.28, whatever this
# system has. Also here: the wasm32 target and the wasm-bindgen CLI (the app
# has the web client inside), GoReleaser, and rpmbuild for COPR's source RPMs.
#
# Windows (x64, the MSVC target, as Tauri wants): cargo-xwin compiles and
# links with clang and LLVM against Microsoft's C runtime and Windows SDK,
# which it downloads from Microsoft into the image (versions pinned below);
# building the image accepts Microsoft's license for them
# (https://go.microsoft.com/fwlink/?LinkId=2086102). NASM for aws-lc's
# assembly, NSIS for the installers (packaging/windows).
#
# Upgrading a tool: change its version and checksums together (the
# checksums come from the tool's own release page).

# Rust itself, from its official image: its toolchain runs on any glibc
# since 2.17, so it is copied into the build machine below.
FROM rust:1.99-slim-trixie AS rust

# What the desktop app links against, for arm64: Ubuntu 22.04's own
# WebKitGTK and GTK with their headers, installed in an arm64 Ubuntu (Docker
# emulates it) and copied below as /sysroot/arm64, which the app's arm64
# build compiles and links against (.goreleaser.yaml). Ubuntu's development
# packages for amd64 and arm64 can't be installed side by side.
FROM --platform=linux/arm64 ubuntu:22.04 AS arm64-sysroot
# Its links made relative (symlinks -c): absolute ones would point outside
# the copy, and the linker would pass over the libraries they name.
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends libc6-dev libwebkit2gtk-4.1-dev symlinks \
 && symlinks -rc /usr/lib > /dev/null \
 && rm -rf /var/lib/apt/lists/*

FROM ubuntu:22.04

ARG TARGETARCH

ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo

RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      ca-certificates curl xz-utils git openssh-client rpm gcc libc6-dev pkg-config \
      libwebkit2gtk-4.1-dev gcc-aarch64-linux-gnu clang lld llvm nasm nsis \
 && rm -rf /var/lib/apt/lists/*

COPY --from=arm64-sysroot /usr/include /sysroot/arm64/usr/include
COPY --from=arm64-sysroot /usr/lib /sysroot/arm64/usr/lib
COPY --from=arm64-sysroot /usr/share/pkgconfig /sysroot/arm64/usr/share/pkgconfig
RUN ln -s usr/lib /sysroot/arm64/lib

RUN rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-pc-windows-msvc wasm32-unknown-unknown

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

# cargo-zigbuild for the CLI; cargo-xwin for Windows; the wasm-bindgen CLI
# the web client's crates pin (packaging/run.sh passes it from
# crates/web/Cargo.toml).
ARG CARGO_ZIGBUILD_VERSION=0.24.0
ARG CARGO_XWIN_VERSION=0.23.1
ARG WASM_BINDGEN_VERSION
RUN test -n "$WASM_BINDGEN_VERSION" \
 && cargo install cargo-zigbuild --version "$CARGO_ZIGBUILD_VERSION" --locked \
 && cargo install cargo-xwin --version "$CARGO_XWIN_VERSION" --locked \
 && cargo install wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION" --locked \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/.package-cache

# Microsoft's C runtime and Windows SDK for cargo-xwin, downloaded now (the
# builds never download them) into XWIN_CACHE_DIR: those of Visual Studio
# 2022 (17), whose last release, 17.14, has MSVC 14.44. xwin checks each
# file against Microsoft's manifest; a version the manifest no longer has
# fails here. Upgrading: change the versions together.
ENV XWIN_CACHE_DIR=/opt/xwin \
    XWIN_VERSION=17 XWIN_CRT_VERSION=14.44.17.14 XWIN_SDK_VERSION=10.0.26100
RUN cargo xwin env --target x86_64-pc-windows-msvc > /dev/null

# Commands run as the developer's own user (packaging/run.sh), so files they
# write in the checkout are theirs: let any user use the toolchain, and
# cargo-xwin its folder.
RUN chmod -R a+w /usr/local/cargo /usr/local/rustup /opt/xwin
