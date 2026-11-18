FROM rust:1-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# Built-in skills are embedded into the binary with include_str!.
COPY builtin_skills ./builtin_skills
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl jq \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home agent \
    && mkdir /workspace \
    && chown agent /workspace
COPY --from=builder /build/target/release/agent /usr/local/bin/agent
USER agent
WORKDIR /workspace
# We already run in an isolated container: no sandbox-exec, `execute` runs without asking.
ENV AGENT_ISOLATED=1
ENTRYPOINT ["agent"]
CMD ["--acp"]
