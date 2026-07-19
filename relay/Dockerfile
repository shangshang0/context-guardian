FROM rust:1.97-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release --bin context-relay-server

FROM debian:bookworm-slim
RUN groupadd --system --gid 65532 relay && useradd --system --uid 65532 --gid 65532 --no-create-home --shell /usr/sbin/nologin relay
COPY --from=build /src/target/release/context-relay-server /usr/local/bin/context-relay-server
ENV CONTEXT_RELAY_LISTEN=0.0.0.0:8080
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/context-relay-server"]
