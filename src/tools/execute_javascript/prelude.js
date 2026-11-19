// Evaluated once per run; returns a function that receives the native host functions
// and installs the script-facing globals (`console`, `fetch`, `fs`, `runScript`). The
// natives are not reachable from the script afterwards.
(function (native) {
  const show = (value) => {
    if (typeof value === "string") return value;
    if (value instanceof Error) return value.stack || String(value);
    try {
      const json = JSON.stringify(value);
      return json === undefined ? String(value) : json;
    } catch (_) {
      return String(value);
    }
  };
  const log = (level) => (...args) => native.log(level, args.map(show).join(" "));
  globalThis.console = { log: log("log"), info: log("info"), warn: log("warn"), error: log("error") };

  globalThis.fs = {
    readText: (path) => native.readText(String(path)),
    writeText: (path, text) => native.writeText(String(path), String(text)),
    list: (path) => native.list(String(path)),
    exists: (path) => native.exists(String(path)),
  };

  // Runs a saved script in-process: its text is the body of an async function that sees
  // `args`, may `await` and `return` a value. The script cannot start processes itself.
  const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor;
  globalThis.runScript = (path, args = []) => new AsyncFunction("args", fs.readText(path))(args);

  // `form` values: string, number, boolean, { file, filename?, type? } (a file in `work`)
  // or an array of those. `saveTo` writes a successful response body to a file in `work`.
  globalThis.fetch = async function fetch(url, options = {}) {
    const headers = {};
    for (const [name, value] of Object.entries(options.headers ?? {})) headers[name] = String(value);
    const raw = JSON.parse(
      native.fetch(
        String(url),
        JSON.stringify({
          method: String(options.method ?? "GET"),
          headers,
          body: options.body == null ? null : String(options.body),
          secret: options.secret == null ? null : String(options.secret),
          form: options.form ?? null,
          saveTo: options.saveTo == null ? null : String(options.saveTo),
        }),
      ),
    );
    return {
      status: raw.status,
      ok: raw.status >= 200 && raw.status < 300,
      headers: raw.headers,
      saved: raw.saved,
      text: async () => raw.body,
      json: async () => JSON.parse(raw.body),
    };
  };
});
