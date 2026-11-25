import { get, post } from "./api.js";

async function json(response) {
  const data = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(data.error || `HTTP ${response.status}`);
  return data;
}

export const list = () => get("/hooks").then(json);
export const save = (fields) => post("/hooks/save", fields).then(json);
export const renewToken = (name) => post("/hooks/token", { name }).then(json);
export const remove = (name) => post("/hooks/delete", { name }).then(json);

export const urlOf = (name) => `${location.origin}/hooks/${name}`;

export const curl = (name, token) =>
  `curl -X POST ${urlOf(name)} \\\n  -H "Authorization: Bearer ${token}" \\\n  -H "Content-Type: application/json" \\\n  -d '{"example": "payload"}'`;

export const blank = () => ({
  create: true,
  name: "",
  enabled: true,
  template: "",
  tools: [],
  skills: [],
  hosts: "",
  writes: "approve",
  callback_url: "",
  timeout_minutes: 10,
});

/** The form's state for an existing hook as the server lists it. */
export const toForm = (hook) => ({
  create: false,
  name: hook.name,
  enabled: hook.enabled,
  template: hook.template,
  tools: [...hook.tools],
  skills: [...hook.skills],
  hosts: hook.hosts.join("\n"),
  writes: hook.writes,
  callback_url: hook.callback_url ?? "",
  timeout_minutes: hook.timeout_minutes,
});

/** The request body for the form (hosts are one per line or comma separated). */
export const toBody = (form) => ({
  ...form,
  hosts: form.hosts.split(/[\n,]/).map((h) => h.trim()).filter(Boolean),
});

/** The form for a built-in workflow that has no hook yet: review it, then save. */
export const fromBuiltin = (workflow) => ({
  ...blank(),
  name: workflow.name,
  template: workflow.template,
  tools: [...workflow.tools],
  skills: [...workflow.skills],
  hosts: (workflow.hosts ?? []).join("\n"),
  writes: workflow.writes,
  timeout_minutes: workflow.timeout_minutes,
});
