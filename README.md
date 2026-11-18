# agent

LLM agent harness with a terminal UI, a browser UI and an [ACP](https://agentclientprotocol.com) server.
Commands run inside a container; the workspace is the only host directory it sees.

## Run in a container

```sh
cp .env.example .env          # in your workspace; fill in LLM_BACKEND, API key, ...
/path/to/agent/agent.sh       # terminal UI
/path/to/agent/agent.sh --web # browser UI
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

## `AGENTS.md`

If the working directory contains an [`AGENTS.md`](https://agents.md), its contents are added to the system prompt (of the main agent and its subagents) on startup. Restart the agent after editing it.

## Browser UI

`agent.sh --web` prints a URL like `http://localhost:8080/#token=...`; open it. The page streams the answer as markdown, shows each tool call as a card with all of its arguments (e.g. the full script of an `execute_javascript` call) and its output (open while pending, running or failed, folded once done) and asks for approval with Allow/Deny buttons where the terminal UI would ask `y/N`. Stop cancels the running prompt. Reloading the page or opening a second tab shows the same session.

- The agent can run commands, so the API requires the random token printed at startup (the page keeps it in `sessionStorage` and removes it from the address bar). The script publishes the port on `127.0.0.1` only; `AGENT_WEB_PORT=9000` changes it.
- Without the script: `agent --web [addr]` (default `127.0.0.1:8080`). In a container use `agent --web 0.0.0.0:8080` with `-p 127.0.0.1:8080:8080`.
- The page is a single file without external resources ([`src/frontend/web.html`](src/frontend/web.html)), so it works offline.

## Context usage

After each model call of the main agent the UIs show how full its context is, e.g. `42k / 200k tokens (21%)`: the terminal UI at the right of the status line (yellow from 80 %, red from 95 %), the browser UI in the header, and ACP clients get a `usage_update` session update (`used`, `size`). The tokens are the input and output of the latest call as the provider reports them, i.e. what the next request sends again. Subagents have their own context and are not counted.

The context window comes from rig's built-in model catalog (where the provider caps input below the window, that lower limit). Models it doesn't know, which includes most Ollama models, show only the tokens, and ACP clients get no update; set `AGENT_CONTEXT_WINDOW=32768` in `.env` to give the size. For Ollama, use the context length the daemon runs the model with (`num_ctx`, `OLLAMA_CONTEXT_LENGTH`), not the model's maximum.

## Skills

Skills ([Agent Skills](https://agentskills.io/specification) format) live in `skills/<name>/`. Only their names and descriptions go into the system prompt; the `use_skill` tool loads a skill's `SKILL.md` when a task needs it, and with `file` (e.g. `references/search.md`) a further document the instructions name, so long reference material is only read when needed.

Built-in skills are embedded in the binary from [`builtin_skills/`](builtin_skills) and reinstalled into `skills/` on every start (edit them in `builtin_skills/`):

- `creating-skills`: how to write a new skill.
- `paperless`: works with a [Paperless-ngx](https://docs.paperless-ngx.com) archive (search and answer from documents, inbox triage, custom fields, bulk edits, upload/download, reports, cleanup) through a JavaScript client for `execute_javascript`. Setup in `.env`: `AGENT_SECRET_PAPERLESS=<host> Token <api token>` and, to skip approvals, the host in `AGENT_JS_ALLOWED_HOSTS`; the agent asks for the URL once and saves it in `work/paperless.json`. Written for Paperless-ngx 3.x (API version 10).

## `view_image` tool

`view_image(path)` shows a PNG, JPEG, GIF or WebP file (max 5 MiB) to the model, e.g. a screenshot or chart made with `execute`. The type is detected from the file's content, so other files (such as `.env`) are rejected. It is built by default (Cargo feature `image`; opt out with `cargo build --no-default-features --features javascript`). Relative paths are relative to the `work/` directory (a subdirectory of the harness's directory, shared with `execute_javascript`); images inside it (after resolving `..` and symlinks) are shown without asking, and in a container all are; elsewhere each call needs approval, as the file is sent to the model provider.

The model must accept images: Anthropic and OpenAI models do; with Ollama use a vision model (e.g. `LLM_MODEL=llama3.2-vision`), as the default `qwen3` is text-only. Ollama cannot carry images inside tool results, so rig attaches them to a following user message instead.

## `execute_javascript` tool

For workflow automation (scripts that call APIs and transform data) there is a tool that runs JavaScript in-process with [QuickJS (rquickjs)](https://crates.io/crates/rquickjs). It is built by default (Cargo feature `javascript`). To opt out:

```sh
cargo build --no-default-features
```

The script is an ES module (top-level `await` works) and gets only this API: no `require`/`import`, no npm, no timers, no process or environment access.

- `console.log/info/warn/error`: captured and returned, together with the error if the script throws.
- `fetch(url, {method, headers, body, secret, form, saveTo})`: resolves to `{status, ok, headers, text(), json()}`. Only `https://` (and `http://` for `localhost`, `127.0.0.1`, `host.docker.internal` and `host.containers.internal`) URLs to hosts in the call's `hosts` argument work; redirects are not followed. For binary files: `form: {title: "x", document: {file: "in/scan.pdf"}}` sends multipart/form-data with the bytes of files in `work/`, and `saveTo: "out/doc.pdf"` writes a successful response body to a file in `work/` instead of returning it (up to 100 MB each).
- `fs.readText(path)`, `fs.writeText(path, text)`, `fs.list(path)`, `fs.exists(path)`: paths relative to the `work/` directory (a subdirectory of the harness's directory, created on first use); anything outside it (absolute paths, `..`, symlinks) and `.env*` files are rejected.

A script cannot start a process, but it can save another script with `fs.writeText` and run it in-process with arguments using `runScript(path, args)`:

```js
fs.writeText("greet.js", "console.log('hello', args.join(' ')); return args.length;");
await runScript("greet.js", ["a", "b"]); // prints "hello a b", resolves to 2
```

Each call runs in a fresh runtime with a 256 MB memory limit and a timeout (`timeout_seconds`, default 30, max 300).

Configuration in `.env`:

- `AGENT_JS_ALLOWED_HOSTS=api.github.com,example.com`: calls whose `hosts` are all in this list (or empty) run without asking; any other host needs the user's approval. This does not depend on `AGENT_ISOLATED`.
- `AGENT_SECRET_<NAME>=<host> <Authorization header value>`, e.g. `AGENT_SECRET_GITHUB=api.github.com Bearer ghp_xxx`: `fetch(url, {secret: "GITHUB"})` makes the harness add that `Authorization` header. The script never sees the value, and the secret is only sent to its host. A server that echoes request headers in its response can still reveal it.
