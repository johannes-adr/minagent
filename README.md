# agent

LLM agent harness with a terminal UI and an [ACP](https://agentclientprotocol.com) server.
Commands run inside a container; the workspace is the only host directory it sees.

## Run in a container

```sh
cp .env.example .env          # in your workspace; fill in LLM_BACKEND, API key, ...
/path/to/agent/agent.sh       # terminal UI
/path/to/agent/agent.sh --acp # ACP over stdio
```

[`agent.sh`](agent.sh) rebuilds the image only when its sources changed, then runs it with the
current directory as workspace (podman if installed, else docker; override with
`AGENT_RUNTIME=docker`). For an ACP client, use it as the agent command, e.g. Zed `settings.json`:

```json
"agent_servers": {
  "agent": { "command": "/path/to/agent/agent.sh", "args": ["--acp"] }
}
```

Without the script:

```sh
docker build -t agent .
docker run --rm -it -v "$PWD:$PWD" -w "$PWD" agent        # TUI (needs -t)
docker run --rm -i  -v "$PWD:$PWD" -w "$PWD" agent --acp  # ACP (no -t, it would garble the protocol)
```

Notes:

- The workspace is mounted under its host path, so the client's `cwd` exists in the container. The client must start the agent in the project directory.
- Config comes from `.env` in the workspace (or `-e LLM_BACKEND=... -e ANTHROPIC_API_KEY=...` without the script).
- Ollama on the host: `OLLAMA_API_BASE_URL=http://host.docker.internal:11434` (podman: `host.containers.internal`).
- Podman: add `--userns=keep-id` so files keep your user (the script does). Docker on Linux: add `--user "$(id -u):$(id -g)"`.
- The container has full network access, and commands in it can read `.env`. Use keys with minimal scope.
