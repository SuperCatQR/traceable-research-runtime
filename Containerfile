FROM docker.io/library/rust:1.96-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM docker.io/library/debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home app \
 && mkdir -p /data \
 && chown app:app /data
COPY --from=build /app/target/release/traceable-search /usr/local/bin/traceable-search
USER app
ENV WEB_BIND=0.0.0.0:8787
ENV TRACEABLE_SEARCH_DATA_DIR=/data
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/traceable-search"]
