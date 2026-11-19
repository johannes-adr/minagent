import { get, post } from "./api.js";

async function json(response) {
  const data = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(data.error || `HTTP ${response.status}`);
  return data;
}

export const list = () => get("/approvals").then(json).then((data) => data.approvals);

/** Approves or declines the call a webhook run waits at; the run goes on in the background. */
export const decide = (session, approve, comment) =>
  post("/approvals/decide", { session, approve, comment }).then(json);

/** The script as a fenced block, with a fence longer than any run of backticks inside it. */
export function fenced(code) {
  const longest = Math.max(2, ...(String(code).match(/`+/g) ?? []).map((run) => run.length));
  const fence = "`".repeat(longest + 1);
  return `${fence}js\n${code}\n${fence}`;
}
