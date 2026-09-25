# SPDX-License-Identifier: Apache-2.0
FROM rust:1.98.0-bookworm AS build
WORKDIR /build
COPY . .
RUN cargo test --locked -p munarium-datastore --test round_trip --no-run && \
    find target/debug/deps -maxdepth 1 -type f -name 'round_trip-*' -executable -exec cp '{}' /fixture \; && \
    /fixture --exact restricted_filesystem::prepare --ignored --nocapture

FROM debian:bookworm-slim
COPY --from=build /fixture /fixture
COPY --from=build /qualification /qualification
USER 65532:65532
ENTRYPOINT ["timeout", "60", "/fixture", "--exact", "restricted_filesystem::serve", "--ignored", "--nocapture"]
