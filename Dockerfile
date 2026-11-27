# kall-server image. Build from the repo root:  docker build -t kall-server .
FROM rust:1.99.1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/proto crates/proto
COPY crates/server crates/server
# The server needs only proto and server: rewrite the workspace to just those (not the client,
# Rune or the FFmpeg wrapper), so new client-side crates never break this build.
RUN sed -i -e 's/^members = .*/members = ["crates\/proto", "crates\/server"]/'            -e 's/^default-members = .*/default-members = ["crates\/server"]/'            -e '/^rune/d' -e '/^\[patch.crates-io\]/,$d' Cargo.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p kall-server && cp target/release/kall-server /kall-server && mkdir /data-empty

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /kall-server /usr/local/bin/kall-server
COPY --from=build --chown=65532:65532 /data-empty /data
ENV KALL_DATA=/data KALL_BIND=0.0.0.0:7744
VOLUME /data
EXPOSE 7744/udp
ENTRYPOINT ["/usr/local/bin/kall-server"]
CMD ["serve"]
