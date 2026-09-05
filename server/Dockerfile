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

FROM rust:1-alpine@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce AS builder
RUN apk add --no-cache musl-dev \
 && rustup target add x86_64-unknown-linux-musl
WORKDIR /build
COPY . .
# FEATURES: extra cargo features for the server build. `vector-diskann` is ON
# by default. The engine stays inert until a direct build's chunk count crosses
# MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD; artifacts below it remain
# exact, and per-collection rollback is a selector write.
ARG FEATURES="vector-diskann"
RUN --mount=type=cache,id=munarium-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=munarium-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=munarium-target,target=/build/target \
    cargo build --release --target x86_64-unknown-linux-musl -p munarium-server -p munarium-cli \
        ${FEATURES:+--features "$FEATURES"} \
 && mkdir -p /out \
 && cp target/x86_64-unknown-linux-musl/release/munarium-server \
       target/x86_64-unknown-linux-musl/release/mmctl /out/

FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=builder /out/munarium-server /munarium-server
COPY --from=builder /out/mmctl /mmctl
USER nonroot
# 8080 REST+docs, 50051 direct gRPC, 9090 ops/metrics
EXPOSE 8080 50051 9090
ENTRYPOINT ["/munarium-server"]
