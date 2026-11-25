# Sorting many existing documents

The user asks in the chat to sort a whole set of documents that are already in Paperless
("sort all old documents", "do the consume workflow for everything in the inbox"). You
orchestrate: a script starts one subagent per document with `agent()`, which proposes its
fields (as a webhook run would); you keep the names consistent across documents, and the user
approves one writing call per wave of about 10 documents. The loop is code, so your own context
only gets what the script prints. Scripts have `agent()` where you have the `agent` tool;
without either, do the inbox triage in `references/recipes.md` in batches of 5.

Progress lives in `work/paperless-backfill.json`, so a later chat can continue where this one
stopped ("continue the Paperless backfill").

## 1. Scope (read-only)

One script: the candidates, a few examples, earlier progress, whether conventions exist.

```js
import { connect } from "skill:paperless/scripts/paperless.js";
const pl = connect();
const state = fs.exists("paperless-backfill.json") ? JSON.parse(fs.readText("paperless-backfill.json")) : null;
const sets = await Promise.all([pl.ids({ inbox: true }), pl.ids({ correspondent: null }), pl.ids({ documentType: null })]);
const ids = [...new Set(sets.flat())].sort((a, b) => a - b);
const { documents } = ids.length ? await pl.find({ ids: ids.slice(0, 8) }) : { documents: [] };
console.log(ids.length, "candidates;", state ? `${Object.keys(state.done).length} of ${state.ids.length} done in an earlier backfill` : "no earlier backfill");
console.log(documents.map((d) => `#${d.id} ${d.title} | ${d.correspondent ?? "-"} | ${d.document_type ?? "-"} | ${d.tags.join(", ")}`).join("\n"));
console.log(fs.exists("paperless-conventions.md") ? "conventions: yes" : "conventions: missing");
```

- The filter above (inbox tag, or no correspondent, or no type) is the default. If the user
  named another set ("everything from 2023", "all with tag Scan"), use that instead.
- Ask the user (`ask_user`) before starting: the count, the examples, the order (oldest first
  unless they say otherwise), and, if `paperless-conventions.md` is missing, whether they want
  to give rules now (title scheme, which tags, how correspondents are named). Whatever they
  say goes into `rules` below and into every briefing.
- If an earlier backfill exists, continue it (the ids not in `done`) unless the user wants a
  new one.

Then save the state (a local file: no `writes` needed):

```js
fs.writeText("paperless-backfill.json", JSON.stringify({ filter: "inbox or no correspondent or no type", ids: [12, 15, 31], done: {}, rules: "Titel: <Art> <Monat> <Jahr>" }, null, 1));
```

`done` maps each finished id to `"ok"`, `"skipped: <why>"` or `"failed: <why>"`.

## 2. One wave

Start with a pilot wave of 5 and ask the user whether the result fits; add their remarks to
`rules`. Then waves of 10.

### a. Proposals: one read-only script per wave

One `execute_javascript` call without `writes` (`hosts: [<paperless host>]`,
`timeout_seconds: 300`; the time subagents take does not count) runs the whole wave. Each
subagent gets the new names that earlier documents of this backfill proposed, so it reuses
them instead of inventing a variant. The script saves the proposals and prints only what you
need: the proposals, `check()` for the new names, and groups of names that look alike.

```js
import { connect, similarGroups } from "skill:paperless/scripts/paperless.js";
const pl = connect();
const state = JSON.parse(fs.readText("paperless-backfill.json"));
const wave = state.ids.filter((id) => !(id in state.done)).slice(0, 10); // the pilot: 5
const pending = state.pending ?? []; // "tag Strom", "correspondent Stadtwerke", "documentType Rechnung"
const proposals = [];
for (const id of wave) {
  const briefing = `Load the skill paperless (use_skill) and its references/consume.md. For Paperless document #${id}, do only steps 1 and 2 (gather context, decide); change nothing.
Rules of the user: ${state.rules || "none beyond work/paperless-conventions.md"}.
New names proposed for earlier documents of this backfill (not created yet, so check() calls them NEW): ${pending.join("; ") || "none"}. Use one of these or an existing name when it fits; propose another new name only when none fits.
Your final report is one line of JSON and nothing else:
{"id": ${id}, "modified": "<doc.modified>", "skip": false, "why": "", "changes": {"title": "...", "correspondent": "...", "documentType": "...", "addTags": ["..."], "created": "YYYY-MM-DD", "customFields": {"Betrag": 84.2}, "removeInboxTags": true}, "note": "", "new": ["tag Strom"], "uncertain": ""}
"changes" uses the keys of update() and leaves out what stays as it is; "skip": true with "why" when the document needs nothing; "new" lists every name that is NEW (as "tag <name>", "correspondent <name>" or "documentType <name>"), including the ones from the list above.`;
  let p;
  try {
    const report = await agent(briefing, { description: `propose #${id}` });
    p = JSON.parse(report.trim().replace(/^```(?:json)?\s*|\s*```$/g, ""));
  } catch (e) {
    p = { id, failed: String(e.message ?? e) };
  }
  for (const n of p.new ?? []) if (!pending.includes(n)) pending.push(n);
  proposals.push(p);
}
Object.assign(state, { pending, proposals });
fs.writeText("paperless-backfill.json", JSON.stringify(state, null, 1));
const names = (kind) => pending.filter((n) => n.startsWith(`${kind} `)).map((n) => n.slice(kind.length + 1));
console.log(proposals.map((p) => JSON.stringify(p)).join("\n"));
console.log(await pl.check({ tags: names("tag"), correspondent: names("correspondent"), documentType: names("documentType") }));
for (const kind of ["tag", "correspondent", "documentType"]) {
  for (const group of similarGroups(names(kind))) console.log("same?", kind, JSON.stringify(group));
}
```

A proposal with `failed` (no report, or not JSON) goes into the next wave once more; after a
second failure mark the document `failed` and go on.

### b. Decide what is left

Only the new names need your attention, not every proposal:

- `check()` says `exists as "<name>"`: write that spelling; the name is not NEW.
- Each `same?` group: one name for all of them.
- Read the NEW names that are left together: synonyms the code can't see ("Auto" and "KFZ",
  "Handy" and "Mobilfunk"), or a new name that means an existing one (`check` lists similar
  existing names). One name each.
- Same correspondent, same title scheme and tags, unless the documents really differ.

Change the proposals yourself (in the table and the writing call); no new subagent is needed.

### c. Table, then the writing call

One row per document, NEW objects in bold, skipped ones in a line below with the reason:

| # | Title | Correspondent | Type | Tags | Created | Other |
|---|---|---|---|---|---|---|
| 101 | Stromrechnung März 2025 | Stadtwerke | Rechnung | Strom, **Steuer 2025** | 2025-03-14 | Betrag 84.20 |

Then directly the writing call (`execute_javascript`, `writes: true`, `timeout_seconds: 300`).
Do not ask for approval in text; the harness asks the user. Create the NEW objects first,
then update each document from a literal list, without `create: true` (a misspelled name
then fails for that document instead of creating a new object). The loop goes over that
literal list only: nothing is read to decide anything.

```js
import { connect } from "skill:paperless/scripts/paperless.js";
const pl = connect();
await pl.createObject("tag", { name: "Steuer 2025" });
const wave = [
  { id: 101, modified: "2025-03-14T10:12:00.123456+01:00", changes: { title: "Stromrechnung März 2025", correspondent: "Stadtwerke", documentType: "Rechnung", addTags: ["Strom", "Steuer 2025"], created: "2025-03-14", customFields: { Betrag: 84.2 }, removeInboxTags: true } },
  { id: 102, modified: "2025-03-20T08:01:44.5+01:00", changes: { title: "Kfz-Versicherung 2025", removeInboxTags: true }, note: "Gehört zu #40" },
];
for (const { id, modified, changes, note } of wave) {
  try {
    await pl.update(id, changes, { unchangedSince: modified });
    if (note) await pl.addNote(id, note);
    console.log(`#${id} ok`);
  } catch (e) {
    console.log(`#${id} FAILED ${e.message}`);
  }
}
```

### d. Check and record

Read the wave again (`find({ ids: [...] })`, no `writes`), compare it with the table, and
update the state file: `done` for each document, and `pending` and `proposals` emptied (the new
names exist now, so `check()` finds them). A document that failed (for example "was changed
since you read it") goes into the next wave with a new proposal.

If the user declines the call, their comment says what to change: fix the proposals, add
lasting rules to `rules`, and make the writing call again.

## 3. Pace

- Keep your messages short between waves; the tables are what the user reads.
- Go on wave by wave while the user approves. Each wave costs you a script, a table and a
  writing call, so your context grows slowly; if it gets long anyway, stop with a report
  (done, skipped, failed, left): a new message or a new chat continues from the state file.
- At the end: suggest matching rules for frequent correspondents (`references/automation.md`)
  so new documents arrive sorted. If the user gave rules in this chat, offer to write them into
  `work/paperless-conventions.md` (it is the user's file: only when they ask, with the text
  they approved).
