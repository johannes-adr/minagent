# agent

LLM agent harness with a terminal UI, a browser UI and an [ACP](https://agentclientprotocol.com) server.

## Install

Latest release (macOS arm64 or Linux x86_64) into the current directory:

```sh
p=$([ "$(uname)" = Darwin ] && echo macos-arm64 || echo linux-x86_64)
t=$(curl -fsSL https://api.github.com/repos/johannes-adr/minagent/releases/latest | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
curl -fsSL "https://github.com/johannes-adr/minagent/releases/download/$t/agent-$t-$p.tar.gz" | tar -xz
```

[`install.sh`](install.sh) does the same and verifies the checksum. Then start the browser UI:

```sh
./agent --web        # http://localhost:8080 (another address: ./agent --web 0.0.0.0:9000)
```

Open the URL, choose a password, and enter the model and API key on the settings page (⚙). Without arguments `./agent` starts the terminal UI, `./agent --acp` the ACP server (stdio). `./agent --version` prints the version.

## Run in a container

```sh
cp .env.example .env          # in your workspace; fill in LLM_BACKEND, API key, ...
/path/to/agent/agent.sh --web # browser UI (or no argument: terminal UI, --acp: ACP over stdio)
```

[`agent.sh`](agent.sh) rebuilds the image when its sources changed and runs it with the current directory as workspace, the only host directory the container sees (podman if installed, else docker; `AGENT_RUNTIME=docker` overrides; `AGENT_WEB_PORT=9000` changes the port, which is published on `127.0.0.1` only). As an ACP client's agent command, e.g. Zed `settings.json`: `"agent_servers": {"agent": {"command": "/path/to/agent/agent.sh", "args": ["--acp"]}}`. Without the script: `docker build -t agent .`, then `docker run --rm -it -v "$PWD:$PWD" -w "$PWD" agent` (the TUI needs `-t`, ACP must not have it).

- The workspace is mounted under its host path, so the client's `cwd` exists in the container.
- Config comes from `config.json` (written by the settings page), `.env` or the environment; the first has priority.
- Ollama on the host: `OLLAMA_API_BASE_URL=http://host.docker.internal:11434` (podman: `host.containers.internal`).
- Commands in the container can read `.env` and `config.json`: use keys with minimal scope.

If the working directory has an [`AGENTS.md`](https://agents.md), it is added to the system prompt on startup.

## Browser UI

The agent can run commands, so the API needs a login: one user, one password, chosen on first visit (only an Argon2id hash is stored, in `auth.json`; delete the file to set a new one). Whoever reaches the page first sets it, so do that before exposing the port. A restart logs you out.

- **Chat:** streams markdown, shows each tool call as a card with all arguments and output, asks for approval where the terminal would ask `y/N`. Subagents get their own tab. ☰ lists saved sessions (`sessions/<id>.json`). The header shows how full the context is (`AGENT_CONTEXT_WINDOW` gives the size for models the catalog does not know; for Ollama the context length the daemon really uses).
- **Settings (⚙):** every option of `.env.example`, saved to `config.json` and applied to the next message; secrets are never sent back to the page. Tools and Skills menus switch tools and skills off for the current chat.
- **Files (🗂):** edit `skills/` and the working directory `work/` (nothing else is reachable).
- **Webhooks (⚡):** an outside service starts the agent with `POST /hooks/<name>`; writes wait for your approval on the Approvals page. Built-in workflows (`builtin_workflows/`, e.g. for Paperless) are listed there. See [docs/webhooks.md](docs/webhooks.md).
- **Updates:** the header shows the version. When a newer release exists, an Update button downloads it for this system, checks its SHA-256 sum, replaces the program and restarts it with the same arguments, environment and working directory (you log in again; sessions stay). Refused while webhook runs are queued, in a container (pull the new image), on other systems and for builds from source (`0.1.0`). The sum comes from the same release as the file: it guards against a broken download, not a hijacked release.

The page is a [Svelte](https://svelte.dev) app in [`web/`](web/), built by Vite into `web/dist` and embedded in the binary. `cargo build` needs `npm install && npm run build` in `web/` first, or it serves a placeholder (`agent.sh` and the Dockerfile do this). `npm run dev` in `web/` gives hot reload against a running `agent --web`.

## Skills

Skills ([Agent Skills](https://agentskills.io/specification) format) live in `skills/<name>/`. Only names and descriptions go into the system prompt; the `use_skill` tool loads a skill's `SKILL.md`, and with `file` a further document it names. Built-in skills are embedded from [`builtin_skills/`](builtin_skills) and reinstalled on every start (edit them there, not in `skills/`):

- `creating-skills`: how to write a skill.
- `paperless`: works with a [Paperless-ngx](https://docs.paperless-ngx.com) archive through a JavaScript client. Setup: `AGENT_SECRET_PAPERLESS=<host> Token <api token>` (host without scheme and port); the host in `AGENT_JS_ALLOWED_HOSTS` skips approvals for reading. `references/consume.md` is the workflow for the built-in `paperless-consume` webhook: the agent proposes title, correspondent, type, tags, date and fields as a table and writes them in one call that you approve. For documents already in Paperless, ask in the chat ("sort all old documents"): `references/backfill.md` has the agent propose each one in a subagent and write them in waves of about 10, one approval per wave, with progress in `work/paperless-backfill.json`. Your title scheme and tag rules go into `work/paperless-conventions.md`.

## Tools

- `execute` runs shell commands (asks first unless the agent runs in a container, `AGENT_ISOLATED=1`).
- `execute_javascript` runs scripts in a sandbox (`fetch` to declared hosts, `work/` files, skill modules); calls that change data need `writes: true` and one approval. Details: [src/tools/execute_javascript/README.md](src/tools/execute_javascript/README.md). Opt out: `cargo build --no-default-features`.
- `view_image` shows a PNG, JPEG, GIF or WebP file to the model (needs a vision model; with Ollama e.g. `llama3.2-vision`).
- `use_skill`, `agent` (subagents) and `ask_user`.
