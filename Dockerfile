# SIMesh's image: the tools to build and run SIMesh on a machine that is not
# Linux. `simesh` builds it on first use and runs itself inside it, with the
# directory holding SIMesh mounted at its own path; nothing of SIMesh is copied
# in or built here.
#
# Ubuntu 24.04 because a device package's ELF is dynamically linked against that
# release's libc, libstdc++, zlib and libbsd, and a package runs only on the
# architecture it was built for: the image is pulled for the host's own
# architecture, which is the one `simesh devices refresh` fetches for.
#
# Holds:
#   - python3 with aiohttp and pyyaml (the front, simd, the ether, devices.py)
#     and pytest
#   - Node 22 and npm (the page's Quasar/Vite build; Vite needs a newer Node
#     than Ubuntu ships)
#   - gcc, g++, make and cmake (the chip library and the time shim)
#   - cargo via rustup, with the wasm32 target (the planner, the berlinmesh
#     kind)
#   - the station runtime: libstdc++, zlib, libbsd
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg git \
        python3 python3-aiohttp python3-yaml python3-pytest \
        build-essential cmake pkg-config \
        libstdc++6 zlib1g libbsd0 \
        procps; \
    mkdir -p /etc/apt/keyrings; \
    curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key \
        | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg; \
    echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_22.x nodistro main" \
        > /etc/apt/sources.list.d/nodesource.list; \
    apt-get update; \
    apt-get install -y --no-install-recommends nodejs; \
    rm -rf /var/lib/apt/lists/*

# Rust in a shared location, writable by whatever uid `simesh` runs the
# container as (the host user's, so what it builds in the mounted tree is
# theirs), so cargo can fetch crates at build time.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN set -eux; \
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal; \
    rustup target add wasm32-unknown-unknown; \
    chmod -R a+rwX "$RUSTUP_HOME" "$CARGO_HOME"

# The home `simesh` mounts a named volume on, for npm's and cargo's caches
# between runs; any uid may write it.
RUN mkdir -p /home/simesh && chmod 1777 /home/simesh

EXPOSE 8800/tcp 8800/udp
