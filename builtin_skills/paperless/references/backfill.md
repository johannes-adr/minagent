# Sorting many existing documents

The user asks in the chat to sort a whole set of documents that are already in Paperless
("sort all old documents", "do the consume workflow for everything in the inbox"). You
orchestrate: one subagent per document proposes its fields (as a webhook run would), you keep
the names consistent across documents, and the user approves one writing call per wave of
about 10 documents. This needs the `agent` tool; without it, do the inbox triage in
`references/recipes.md` in batches of 5.

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

### a. A proposal per document

One `agent` call per document; several in one answer is fine (they run one after the
other). The subagent does not see this chat, so the briefing is complete:

> Load the skill paperless (use_skill) and its references/consume.md. For Paperless document
> #<id>, do only steps 1 and 2 (gather context, decide). Change nothing: no call with
> `writes: true`, no notes, no files. Rules of the user: <rules, or "none beyond
> work/paperless-conventions.md">. Your final report is one line of JSON and nothing else:
> `{"id": <id>, "modified": "<doc.modified>", "skip": false, "why": "", "changes": {"title": "...", "correspondent": "...", "documentType": "...", "addTags": ["..."], "created": "YYYY-MM-DD", "customFields": {"Betrag": 84.2}, "removeInboxTags": true}, "note": "", "new": ["tag Strom"], "uncertain": ""}`
> `changes` uses the keys of `update()` and leaves out what stays as it is. `skip: true` with
> `why` when the document needs nothing. `new` lists names that `check` calls NEW.

A report that is not that JSON, or a subagent that failed: run it once more, then mark the
document `failed` and go on.

### b. Make the wave consistent

Subagents of the same wave don't see each other. Before showing anything:

- Check all `new` names of the wave together, read-only:
  `console.log(await pl.check({ tags: [...], correspondent: [...], documentType: [...] }))`.
  A name that `exists as` something is written with that spelling and is not NEW.
- Merge variants between documents: "Versicherung" and "Versicherungen", "Telekom" and
  "Telekom Deutschland GmbH". One name each, in every proposal that uses it.
- Same correspondent, same title scheme and tags, unless the documents really differ.

You can fix proposals yourself; no new subagent is needed for that.

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
update `done` in the state file. A document that failed (for example "was changed since you
read it") goes into the next wave with a new proposal.

If the user declines the call, their comment says what to change: fix the proposals, add
lasting rules to `rules`, and make the writing call again.

## 3. Pace

- Keep your messages short between waves; the tables are what the user reads.
- Go on wave by wave while the user approves. After about 5 waves (50 documents) stop with a
  report: done, skipped, failed, left. Your context fills up, and a new message or a new chat
  continues from the state file.
- At the end: suggest matching rules for frequent correspondents (`references/automation.md`)
  so new documents arrive sorted. If the user gave rules in this chat, offer to write them into
  `work/paperless-conventions.md` (it is the user's file: only when they ask, with the text
  they approved).
