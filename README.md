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
- `fetch(url, {method, headers, body, secret})`: resolves to `{status, ok, headers, text(), json()}`. Only `https://` (and `http://` for `localhost`/`127.0.0.1`) URLs to hosts in the call's `hosts` argument work; redirects are not followed.
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
