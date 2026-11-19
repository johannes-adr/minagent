// The state of the chat: everything the server tells us over the event stream, plus the
// transcript it builds. Components only read `chat` and call the functions exported here.
import { eventsUrl, get, post, token } from "./api.js";

let nextId = 1;

/** A pane is one transcript: the main chat, or the panel of one subagent. */
const newPane = () => ({ items: [], assistant: null, reasoning: null });

export const chat = $state({
  main: newPane(),
  subs: [], // {id, status, description, prompt, pane}
  activeSub: null,
  sideHidden: false,
  busy: false,
  offline: false,
  stateText: "Connecting…",
  context: null,
  thinking: null,
  tools: null, // {available, enabled, default, custom}
  sessions: [],
  current: null,
  drawer: false,
});

let tools = new Map(); // tool call / question key -> reactive item, for all panes

function add(pane, kind, fields = {}) {
  pane.items.push({ id: nextId++, kind, ...fields });
  return pane.items[pane.items.length - 1]; // the reactive proxy, not the plain object
}

const sub = (id) => chat.subs.find((s) => s.id === id);

function subFor(id) {
  let found = sub(id);
  if (!found) {
    chat.subs.push({ id, status: "running", description: "", prompt: "", pane: newPane() });
    found = sub(id);
  }
  return found;
}

/** The pane an event belongs to: events of a subagent carry its id. */
const paneOf = (agent) => (agent ? subFor(agent).pane : chat.main);

function setSubagent(event) {
  const s = subFor(event.agent);
  if (event.description !== undefined) {
    s.description = event.description;
    s.prompt = event.prompt || "";
    chat.sideHidden = false;
    chat.activeSub = event.agent;
  } else if (!chat.activeSub) {
    chat.activeSub = event.agent;
  }
  s.status = event.status;
}

// A stopped prompt never reports its subagents as finished.
function settleSubagents() {
  for (const s of chat.subs) if (s.status === "running") s.status = "cancelled";
}

function resetTranscript() {
  chat.main = newPane();
  chat.subs = [];
  chat.activeSub = null;
  chat.sideHidden = false;
  chat.context = null;
  tools = new Map();
}

function endReasoning(pane) {
  const item = pane.reasoning;
  pane.reasoning = null;
  item.live = false;
  if (!item.touched) item.open = false;
}

function upsertTool(event) {
  let tool = tools.get(event.id);
  if (!tool) {
    tool = add(paneOf(event.agent), "tool", { data: {}, touched: false, open: false });
    tools.set(event.id, tool);
  }
  Object.assign(tool.data, event);
  // Calls that need a decision, run or fail are shown open, so the whole call can be read;
  // finished ones fold away unless the user has opened or closed the card themselves.
  if (!tool.touched) tool.open = ["pending", "running", "failed"].includes(event.status);
}

function upsertQuestion(event) {
  const key = "q:" + event.id;
  let question = tools.get(key);
  if (!question) {
    question = add(chat.main, "question", { data: {}, sent: false });
    tools.set(key, question);
  }
  Object.assign(question.data, event);
}

function handle(event) {
  const pane = paneOf(event.agent);
  if (!["text", "context", "thinking", "subagent", "reasoning"].includes(event.type)) pane.assistant = null;
  // The reasoning block folds away once the model moves on to its answer or a tool call.
  if (pane.reasoning && !["reasoning", "context", "thinking", "subagent"].includes(event.type)) endReasoning(pane);
  switch (event.type) {
    case "user":
      add(chat.main, "user", { text: event.text });
      break;
    case "text":
      if (!pane.assistant) pane.assistant = add(pane, "assistant", { raw: "" });
      pane.assistant.raw += event.text;
      break;
    case "reasoning":
      if (!pane.reasoning) pane.reasoning = add(pane, "reasoning", { text: "", live: true, open: true, touched: false });
      pane.reasoning.text += event.text;
      break;
    case "note":
      add(pane, "note", { text: event.text });
      break;
    case "error":
      add(chat.main, "error", { text: event.text });
      break;
    case "subagent":
      setSubagent(event);
      break;
    case "tool":
      upsertTool(event);
      break;
    case "question":
      upsertQuestion(event);
      break;
    case "status":
      setBusy(event.busy);
      if (!event.busy) settleSubagents();
      break;
    case "context":
      chat.context = event;
      break;
    case "thinking":
      chat.thinking = event.levels.length ? event : null; // no levels: the model has none
      break;
    case "tools":
      chat.tools = event;
      break;
    case "session":
      // Another session is shown: start its transcript over.
      resetTranscript();
      chat.current = event.id;
      loadSessions();
      break;
    case "sessions":
      loadSessions();
      break;
  }
}

function setBusy(busy) {
  chat.busy = busy;
  chat.stateText = busy ? "Working…" : "Ready";
}

// Events are applied in batches, one per animation frame at most: a streamed answer
// arrives token by token and re-rendering its markdown for each one would be wasted work.
let queue = [];
let scheduled = false;
function enqueue(event) {
  queue.push(event);
  if (scheduled) return;
  scheduled = true;
  setTimeout(() => {
    scheduled = false;
    const batch = queue;
    queue = [];
    for (const e of batch) handle(e);
  }, 30);
}

// ---- Sessions ----
export async function loadSessions() {
  const response = await get("/sessions");
  if (!response.ok) return;
  chat.sessions = (await response.json()).sessions;
}

/** Runs a request that is refused while the agent works; returns whether it was accepted. */
async function busyAware(promise) {
  const response = await promise;
  if (response.status === 409) add(chat.main, "note", { text: "Wait for the agent to finish before switching sessions." });
  else if (!response.ok && response.status !== 202) add(chat.main, "error", { text: `Request failed (HTTP ${response.status})` });
  return response.status === 202;
}

export async function openSession(id) {
  if (id !== chat.current && (await busyAware(post("/session", { id })))) chat.drawer = false;
}
export async function newSession() {
  if (await busyAware(post("/session", {}))) chat.drawer = false;
}
export async function deleteSession(id) {
  if (confirm("Delete this session?")) await busyAware(post("/session/delete", { id }));
}

// ---- Actions ----
export async function sendPrompt(text) {
  const response = await post("/prompt", { text });
  if (response.status === 202) return true;
  if (response.status === 409) add(chat.main, "note", { text: "The agent is still working on the previous message." });
  else add(chat.main, "error", { text: `Could not send (HTTP ${response.status})` });
  return false;
}
export const cancel = () => post("/cancel");
export const approve = (id, allow) => post("/approve", { id, allow });
export const answer = (id, text) => post("/answer", { id, answer: text });
export const setThinking = (level) => post("/thinking", { level });
/** Gives this chat its own tools; `null` goes back to the configured ones. */
export const setTools = (tools) => busyAware(post("/tools", { tools }));

// ---- Connection: every (re)connect replays the whole transcript ----
export function connect() {
  const source = new EventSource(eventsUrl());
  source.onopen = () => {
    chat.offline = false;
    queue = [];
    resetTranscript();
    setBusy(false);
  };
  source.onmessage = (message) => enqueue(JSON.parse(message.data));
  source.onerror = () => {
    chat.offline = true;
    chat.stateText =
      source.readyState === EventSource.CLOSED ? "Not connected: open the URL the agent printed" : "Reconnecting…";
    if (source.readyState === EventSource.CLOSED && token) setTimeout(connect, 3000);
  };
}
