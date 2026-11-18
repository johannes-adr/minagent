#!/bin/sh
# Builds the image when its sources changed, then runs the agent with $PWD as workspace.
#
#   /path/to/agent.sh          terminal UI
#   /path/to/agent.sh --acp    ACP over stdio (use as the command in your ACP client)
#   /path/to/agent.sh --web    browser UI; open the URL it prints
#
# AGENT_RUNTIME=docker|podman (default: podman if installed, else docker).
# AGENT_WEB_PORT=8080         host port for --web (published on localhost only).
set -eu
repo=$(cd "$(dirname "$0")" && pwd)
rt=${AGENT_RUNTIME:-$(command -v podman >/dev/null && echo podman || echo docker)}
image=agent
hash=$(cd "$repo" && find Dockerfile Cargo.toml Cargo.lock src builtin_skills -type f | sort | xargs cksum | cksum | cut -d' ' -f1)
built=$("$rt" image inspect -f '{{index .Config.Labels "agent.hash"}}' "$image" 2>/dev/null || true)
if [ "$built" != "$hash" ]; then
    # To stderr: in ACP mode stdout belongs to the protocol.
    "$rt" build -t "$image" --label "agent.hash=$hash" "$repo" >&2
fi
tty=
if [ -t 0 ] && [ -t 1 ]; then tty=-t; fi # ACP must not get a terminal
userns=
if [ "$rt" = podman ]; then userns=--userns=keep-id; fi # files keep your user
if [ "${1:-}" = --web ]; then
    # Listen on all interfaces inside the container, but publish only on the host's localhost.
    port=${AGENT_WEB_PORT:-8080}
    set -- --web "0.0.0.0:$port"
    publish="-p 127.0.0.1:$port:$port"
else
    publish=
fi
exec "$rt" run --rm -i $tty $userns $publish -v "$PWD:$PWD" -w "$PWD" "$image" "$@"
