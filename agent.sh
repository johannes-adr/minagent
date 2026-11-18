#!/bin/sh
# Builds the image when its sources changed, then runs the agent with $PWD as workspace.
#
#   /path/to/agent.sh          terminal UI
#   /path/to/agent.sh --acp    ACP over stdio (use as the command in your ACP client)
#
# AGENT_RUNTIME=docker|podman (default: podman if installed, else docker).
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
exec "$rt" run --rm -i $tty $userns -v "$PWD:$PWD" -w "$PWD" "$image" "$@"
