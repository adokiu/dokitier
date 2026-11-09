FROM node:20-bookworm AS web
WORKDIR /web
COPY web/webui/package*.json ./
RUN npm ci --no-audit --no-fund
COPY web/webui/ ./
RUN npm run build

FROM rust:1.95-bookworm AS rust
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
COPY --from=web /web/dist ./web/webui/dist
RUN cargo build --release -p dokitier-console -p dokitier

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=rust /src/target/release/dokitier-console /usr/local/bin/dokitier-console
COPY --from=rust /src/target/release/dokitier-core /usr/local/bin/dokitier-core
COPY --from=web /web/dist /opt/webui
ENTRYPOINT []
