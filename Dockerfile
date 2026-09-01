# syntax=docker/dockerfile:1.7

# ── Build stage ──────────────────────────────────────────────────────────────
FROM rust:1.88-slim AS builder
WORKDIR /app

# Prime the dependency layer separately from source so incremental builds
# don't rebuild every dependency on every code change.
COPY Cargo.toml Cargo.lock ./
COPY api/Cargo.toml           api/Cargo.toml
COPY engine/Cargo.toml        engine/Cargo.toml
COPY state/Cargo.toml         state/Cargo.toml
COPY types/Cargo.toml         types/Cargo.toml
COPY committer/Cargo.toml     committer/Cargo.toml
COPY committee/Cargo.toml     committee/Cargo.toml
COPY tee/Cargo.toml           tee/Cargo.toml
COPY zkvm/Cargo.toml          zkvm/Cargo.toml
COPY benches/Cargo.toml       benches/Cargo.toml
RUN mkdir -p api/src engine/src state/src types/src committer/src committee/src tee/src zkvm/src benches/benches \
    && echo 'fn main() {}' > api/src/main.rs \
    && for d in engine state types committer committee tee zkvm; do echo '' > $d/src/lib.rs; done \
    && echo 'fn main() {}' > benches/benches/matching.rs \
    && cargo build --release --bin api || true \
    && rm -rf api engine state types committer committee tee zkvm benches

# Now copy the real source and build for real.
COPY . .
RUN cargo build --release --bin api

# ── Runtime stage ────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 vela \
    && useradd  --system --uid 10001 --gid vela --home-dir /app --shell /usr/sbin/nologin vela \
    && mkdir -p /data \
    && chown -R vela:vela /data

COPY --from=builder /app/target/release/api /usr/local/bin/api

USER vela
WORKDIR /app
EXPOSE 3001

# Container-level healthcheck. Fly's [http_service.checks] block is the
# authoritative signal for the platform; this makes `docker ps`/local runs
# also expose health.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3001/health || exit 1

CMD ["api"]
