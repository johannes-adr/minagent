# The web UI: static files that the binary embeds.
FROM node:22-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

FROM rust:1-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY --from=web /web/dist ./web/dist
# Built-in skills are embedded into the binary with include_str!.
COPY builtin_skills ./builtin_skills
# Cache mounts keep the registry and compiled dependencies between builds, so a code
# change only recompiles this crate. `target/` is not part of the image: copy the binary out.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked && cp target/release/agent /agent

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl jq \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home agent \
    && mkdir /workspace \
    && chown agent /workspace
COPY --from=builder /agent /usr/local/bin/agent
USER agent
WORKDIR /workspace
# We run in an isolated container: `execute` runs without asking.
ENV AGENT_ISOLATED=1
# No arguments: terminal UI (`docker run -it`); `--acp`: ACP over stdio (`docker run -i`).
ENTRYPOINT ["agent"]
