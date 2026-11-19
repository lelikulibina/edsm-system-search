FROM rust:1.86-bookworm AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --create-home app
WORKDIR /app
COPY --from=builder /app/target/release/edsm-system-search /usr/local/bin/edsm-system-search
RUN mkdir -p /data && chown app:app /data
USER app
ENV EDSM_DB=/data/systems.db
ENV EDSM_BIND=0.0.0.0:8080
EXPOSE 8080
VOLUME ["/data"]
ENTRYPOINT ["edsm-system-search"]
