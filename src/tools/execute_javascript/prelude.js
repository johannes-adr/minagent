// Evaluated once per run; returns a function that receives the native host functions
// and installs the script-facing globals (`console`, `fetch`, `fs`). The natives are not
// reachable from the script afterwards.
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

  globalThis.fetch = async function fetch(url, options = {}) {
    const headers = {};
    for (const [name, value] of Object.entries(options.headers ?? {})) headers[name] = String(value);
    const raw = JSON.parse(
      native.fetch(
        String(url),
        String(options.method ?? "GET"),
        JSON.stringify(headers),
        options.body == null ? null : String(options.body),
        options.secret == null ? null : String(options.secret),
      ),
    );
    return {
      status: raw.status,
      ok: raw.status >= 200 && raw.status < 300,
      headers: raw.headers,
      text: async () => raw.body,
      json: async () => JSON.parse(raw.body),
    };
  };
});
