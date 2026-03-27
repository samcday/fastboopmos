FROM docker.io/rust:1.93-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

FROM docker.io/debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/frontdoor /usr/local/bin/frontdoor
ENTRYPOINT ["frontdoor"]
