import { post } from "./api.js";

// The file API: every call names a root (`skills` or `work`) and a path inside it.
async function call(op, body) {
  const response = await post("/files/" + op, body);
  const data = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(data.error || `HTTP ${response.status}`);
  return data;
}

export const ROOTS = [
  { id: "skills", label: "Skills", hint: "skills/ — instructions the agent loads on demand" },
  { id: "work", label: "Working directory", hint: "work/ — where the agent keeps its files" },
];

export const list = (root, path) => call("list", { root, path }).then((d) => d.entries);
export const read = (root, path) => call("read", { root, path });
export const write = (root, path, content) => call("write", { root, path, content });
export const mkdir = (root, path) => call("mkdir", { root, path });
export const remove = (root, path) => call("delete", { root, path });
export const rename = (root, from, to) => call("rename", { root, from, to });

export const join = (dir, name) => (dir ? `${dir}/${name}` : name);
export const parent = (path) => path.split("/").slice(0, -1).join("/");
export const base = (path) => path.split("/").pop();

/** The text a new skill starts from (name and description are required by the format). */
export const skillTemplate = (name) => `---
name: ${name}
description: What this skill does and when the agent should use it.
---

# ${name}

Instructions for the agent go here.
`;
