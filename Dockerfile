# muninn.io — one image, two binaries, nothing else it does not need.
#
# Three stages: fetch and verify Telegraf, build muninn, assemble a runtime that
# carries only the two binaries.

# ── Stage 1: Telegraf, fetched by version and verified ───────────────────────
#
# The version and both checksums are recorded in
# docs/adr/0011-telegraf-pinning.md. Bumping Telegraf is a deliberate change to
# three lines here, visible in the diff — it cannot happen by rebuilding.
#
# Why the tarball rather than `COPY --from=telegraf:1.39.2`: the image digest
# pins bytes muninn does not use (an entrypoint, a default config, a user
# setup), so it churns whenever any of them changes. The checksum names exactly
# the artefact that ships. It also keeps the build independent of Docker Hub
# rate limits.
FROM debian:12-slim AS telegraf

ARG TELEGRAF_VERSION=1.39.2
ARG TELEGRAF_SHA256_AMD64=3ecf733bec389b8a0e1072f134ce379d79efe0d3caf984c164bd4cfc515a86d6
ARG TELEGRAF_SHA256_ARM64=7626df978e86b4788aed477f7acb4528ff517b506c721f1bd4c9ac77464a93e5

# Set by buildx per target platform.
ARG TARGETARCH

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl; \
    rm -rf /var/lib/apt/lists/*; \
    case "${TARGETARCH}" in \
        amd64) sha="${TELEGRAF_SHA256_AMD64}" ;; \
        arm64) sha="${TELEGRAF_SHA256_ARM64}" ;; \
        # A new architecture must fail loudly rather than build an image with an
        # unverified binary in it.
        *) echo "no pinned Telegraf checksum for TARGETARCH=${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    file="telegraf-${TELEGRAF_VERSION}_linux_${TARGETARCH}.tar.gz"; \
    curl -fsSL -o "/tmp/${file}" "https://dl.influxdata.com/telegraf/releases/${file}"; \
    echo "${sha}  /tmp/${file}" | sha256sum -c -; \
    tar -xzf "/tmp/${file}" -C /tmp; \
    # The tarball lays out ./telegraf-<version>/usr/bin/telegraf.
    install -m 0755 "/tmp/telegraf-${TELEGRAF_VERSION}/usr/bin/telegraf" /telegraf; \
    /telegraf version

# ── Stage 2: build muninn ────────────────────────────────────────────────────
# Pinned to the MSRV in Cargo.toml. CI runs floating stable, so a dependency
# that raises the MSRV leaves CI green while *this* fails — which is the point:
# the image is the real gate.
FROM rust:1.88-slim AS builder

WORKDIR /build

# Dependencies first, so editing source does not re-download the world.
COPY Cargo.toml Cargo.lock ./
COPY crates/muninn-core/Cargo.toml     crates/muninn-core/Cargo.toml
COPY crates/muninn-telegraf/Cargo.toml crates/muninn-telegraf/Cargo.toml
COPY crates/muninn-modules/Cargo.toml  crates/muninn-modules/Cargo.toml
COPY crates/muninn-health/Cargo.toml   crates/muninn-health/Cargo.toml
COPY muninn/Cargo.toml                 muninn/Cargo.toml

RUN set -eux; \
    mkdir -p crates/muninn-core/src crates/muninn-telegraf/src \
             crates/muninn-modules/src crates/muninn-health/src muninn/src; \
    for c in core telegraf modules health; do \
        echo 'pub fn _stub() {}' > "crates/muninn-${c}/src/lib.rs"; \
    done; \
    echo 'fn main() {}' > muninn/src/main.rs; \
    cargo build --release --locked; \
    rm -rf crates/*/src muninn/src

# The real build. Touch the sources so cargo sees them as newer than the stubs
# above, which share their timestamps with the COPY layer.
COPY . .
RUN set -eux; \
    find . -name '*.rs' -exec touch {} +; \
    cargo build --release --locked -p muninn

# ── Stage 3: runtime ─────────────────────────────────────────────────────────
#
# debian-slim rather than distroless, and that is a deliberate trade with a
# measured cost: 88 packages instead of 10, and a shell and package manager
# inside a container that mounts the host filesystem.
#
# It buys the updates module. Reading the host's package state needs real apt
# and dpkg — the WP1 spike established that, and that no approach without them
# works under the hardening baseline. See docs/hardening.md for the numbers and
# docs/adr/0009-updates-module-approach.md for the reasoning.
#
# Which makes the hardening below load-bearing rather than decoration.
FROM debian:12-slim

# ca-certificates only: Telegraf needs a trust store to reach InfluxDB over
# HTTPS, and without it every write fails with a certificate error that looks
# like a server problem. apt and dpkg are already present in the base and are
# what the updates module uses.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates; \
    rm -rf /var/lib/apt/lists/*

# A fixed uid/gid, because the tmpfs for /run/muninn has to be given the same
# ones — see the compose file. A generated uid would make that line a guess.
ARG MUNINN_UID=10001
ARG MUNINN_GID=10001
RUN set -eux; \
    groupadd --gid "${MUNINN_GID}" muninn; \
    useradd --uid "${MUNINN_UID}" --gid "${MUNINN_GID}" --no-create-home \
            --shell /usr/sbin/nologin muninn

COPY --from=telegraf /telegraf                        /usr/local/bin/telegraf
COPY --from=builder  /build/target/release/muninn     /usr/local/bin/muninn

# Present so a bind mount has somewhere to land and the paths in the docs exist
# even before anything is mounted.
RUN set -eux; \
    mkdir -p /etc/muninn /run/muninn /hostfs; \
    chown "${MUNINN_UID}:${MUNINN_GID}" /run/muninn; \
    chmod 0700 /run/muninn

USER muninn:muninn

# 9273 = host metrics (Telegraf), 8080 = health, status and agent metrics
# (muninn). Two endpoints, two purposes — see docs/configuration.md.
EXPOSE 8080 9273

ENV MUNINN_CONFIG=/etc/muninn/muninn.yaml

# start-period covers generation, `telegraf config check` and the Telegraf
# start; before that the container is starting, not unhealthy.
HEALTHCHECK --interval=30s --timeout=5s --retries=3 --start-period=20s \
    CMD ["/usr/local/bin/muninn", "healthcheck"]

ENTRYPOINT ["/usr/local/bin/muninn"]
CMD ["run"]
