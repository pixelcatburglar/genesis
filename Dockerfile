# Genesis server image. Build context is the repo root; see .dockerignore.
FROM rust:1.97-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY db ./db
RUN cargo build --release --bin genesis

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 genesis
COPY --from=build /src/target/release/genesis /usr/local/bin/genesis
# Static content and game migrations are read at startup (GENESIS_DB_DIR).
COPY db /app/db
WORKDIR /app
USER genesis
# The client's login screen defaults to port 26000.
EXPOSE 26000
ENTRYPOINT ["/usr/local/bin/genesis"]
