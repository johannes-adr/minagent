// System prompt. Run by the agent at startup; whatever this script prints is the system
// prompt (`agent --js system_prompt/system.js <agent-exe>` prints it for inspection).
// ARGS[0] is the path of the agent executable. This directory is only created once, so
// edit or delete anything here.
//
// Every other `<name>.js` / `<name>.md` file in this directory is a component and is
// included automatically, sorted by file name (prefix with numbers like `10-rules.js` to
// order them). A `.md` file is printed as is; a `.js` file's default export is called
// (it may be async) and returns the text. Files starting with `_` are helpers for
// components to import and are skipped. To include a new part, just add a file.
import fs from "node:fs";

const DIR = "system_prompt";

const parts = [
  'You are a helpful agent. Use the `execute` tool to run shell commands and `execute_javascript` to run JavaScript. Both run in a sandbox (writes only in the working directory and temp, no internet, no access to .env files); set `disable_sandbox` (execute) or `allow_network` (execute_javascript) only if a call fails with "Operation not permitted" or needs the network.',
];

for (const file of fs.readdirSync(DIR).sort()) {
  if (file === "system.js" || file.startsWith("_")) continue;
  if (file.endsWith(".md")) {
    parts.push(fs.readFileSync(`${DIR}/${file}`, "utf8"));
  } else if (file.endsWith(".js")) {
    parts.push(String(await (await import(`./${file}`)).default()));
  }
}

console.log(parts.map((p) => p.trim()).filter(Boolean).join("\n\n"));
