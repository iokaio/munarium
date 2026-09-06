# syntax=docker/dockerfile:1
# Multi-stage build: static musl binary -> distroless. Target < 30 MB.
# Build context is server/. Never depends on a live database: all sqlx queries
# are runtime-checked strings (no query! macros), so the build needs neither a
# database nor .sqlx offline data — conformance against both store backends is
# the query-drift net. (An earlier SQLX_OFFLINE=true here was a no-op and is
# gone; if query! macros ever land, add `cargo sqlx prepare` + the env back.)
#
# Build cache: the compile step mounts BuildKit cache volumes for
# the cargo registry/git checkouts and the target directory, so dependency
# artifacts and unchanged workspace crates persist across builds on one
# builder instead of recompiling from zero every time `COPY . .` invalidates
# the layer (a cold build is minutes; a warm one rebuilds only the crates a
# commit touched). The binaries are copied OUT of
# the mount inside the same RUN — a cache mount is not part of the image, so
# the later `COPY --from=builder` must read a real layer path (/out). The
# `id`s keep the volumes stable across tags; `docker builder prune` clears
# them. BuildKit is the default engine on Docker Desktop and GitHub Actions;
# the `# syntax=` line above is required for `--mount`.

# Include Cargo.lock from the public source context in the dependency SBOM.
ARG BUILDKIT_SBOM_SCAN_CONTEXT=true
FROM --platform=$BUILDPLATFORM tonistiigi/xx:1.8.0@sha256:add602d55daca18914838a78221f6bbe4284114b452c86a48f96d59aeb00f5c6 AS cross
FROM --platform=$BUILDPLATFORM rust:1-alpine@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce AS builder
# Compile on the builder's CPU, including C dependencies, for the requested target.
COPY --from=cross / /
ENV RUSTUP_TOOLCHAIN=1.98.0
RUN apk add --no-cache musl-dev clang lld
WORKDIR /build
COPY . .
# FEATURES: extra cargo features for the server build. `vector-diskann` is ON
# by default. The engine stays inert until a direct build's chunk count crosses
# MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD; artifacts below it remain
# exact, and per-collection rollback is a selector write.
ARG FEATURES="vector-diskann"
ARG TARGETARCH
ARG TARGETPLATFORM
ARG BUILDKIT_SBOM_SCAN_STAGE=true
RUN xx-apk add --no-cache musl-dev gcc
RUN --mount=type=cache,id=munarium-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=munarium-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=munarium-target-${TARGETARCH},target=/build/target \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "Unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && xx-cargo build --locked --release -p munarium-server -p munarium-cli \
        ${FEATURES:+--features "$FEATURES"} \
 && mkdir -p /out \
 && cp "target/$target/release/munarium-server" \
       "target/$target/release/mmctl" /out/ \
 && xx-verify --static /out/munarium-server /out/mmctl

FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
ARG SOURCE_REVISION="unknown"
ARG BUILD_VERSION="1.0.0"
LABEL org.opencontainers.image.source="https://github.com/iokaio/munarium" \
      org.opencontainers.image.revision=$SOURCE_REVISION \
      org.opencontainers.image.version=$BUILD_VERSION \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.title="Munarium Server"
COPY --from=builder /out/munarium-server /munarium-server
COPY --from=builder /out/mmctl /mmctl
COPY LICENSE NOTICE THIRD_PARTY_NOTICES.md /usr/share/licenses/munarium/
USER nonroot
# 8080 REST+docs, 50051 direct gRPC, 9090 ops/metrics
EXPOSE 8080 50051 9090
ENTRYPOINT ["/munarium-server"]
