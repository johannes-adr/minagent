---
name: paperless
description: Works with a Paperless-ngx document archive through its REST API - search documents and answer questions from their text, triage the inbox (set title, correspondent, document type, tags, dates, custom fields), extract data such as amounts or invoice numbers, bulk edit, upload or download files, export reports, and clean up tags or correspondents. Use when the user mentions Paperless, paperless-ngx, their document archive, scanned letters, invoices or contracts stored there.
compatibility: JavaScript module for the execute_javascript tool (not Node). Needs the secret PAPERLESS and network access to the Paperless host. Written for Paperless-ngx 3.x (API version 10).
---

# Paperless-ngx

All work goes through one module in `execute_javascript`. A script can do many API calls, so
do a whole step (search, read, decide, write) in one call and print only what you need.

```js
import { connect } from "skill:paperless/scripts/paperless.js";
const pl = connect();
console.log(await pl.check()); // { url, ok, user, serverVersion, apiVersion }
```

Call `execute_javascript` with `hosts: [<the Paperless host>]` (`pl.host` tells it; errors
say it too). Raise `timeout_seconds` (max 300) for jobs over many documents.

## Setup (once)

- **Token:** the harness adds it; scripts never see it. If `check()` reports an unknown secret,
  tell the user to add to `.env` and restart the agent:
  `AGENT_SECRET_PAPERLESS=<host> Token <api token>` (the token is under "My Profile" in the
  Paperless web UI; `<host>` without scheme and port). To run without approval prompts they can
  add the host to `AGENT_JS_ALLOWED_HOSTS`.
- **URL:** `connect()` reads `work/paperless.json`. If it is missing, ask the user for the
  URL as they open Paperless in the browser and save it with
  `await saveUrl(url)` (also exported; it checks the connection before saving). The saved URL
  is the user's setting: never change it, its scheme or its port on your own, and never write
  `paperless.json` with `fs.writeText`. If the connection fails, report the error to the user
  instead of trying other URLs.

## The client

Objects (tags, correspondents, document types, storage paths, custom fields) are given by
**name or id** (numbers are ids, strings are names, case-insensitive). An unknown name throws
with similar names. Documents come back compact: names instead of ids, custom fields as
`{name: value}`, no text unless requested.

| Call | Result |
|---|---|
| `find(filters, {limit = 25, content})` | `{count, documents}`; `limit: Infinity` for all, `content: true` or max characters per document |
| `count(filters)`, `ids(filters)` | number of matches / all matching ids |
| `doc(id, {content = true})` | one document with text (up to 20000 characters by default) and notes |
| `vocabulary()` | all tags (with `inbox`, `parent`), correspondents, document types, storage paths, custom fields (type, options), with document counts and matching rules |
| `update(id, changes, {create})` | changes one document, returns it; `changes`: `title`, `created` (`YYYY-MM-DD`), `asn`, `correspondent`, `documentType`, `storagePath` (null clears), `tags` (replaces), `addTags`, `removeTags`, `customFields` (`{name: value}`, null removes), `removeInboxTags: true`; `create: true` creates missing tags, correspondents, document types |
| `addTags(ids, tags)`, `removeTags(ids, tags)`, `modifyTags(ids, {add, remove})` | bulk edit, `{task_id}` |
| `setCorrespondent(ids, x)`, `setDocumentType(ids, x)`, `setStoragePath(ids, x)`, `setCustomFields(ids, {name: value})` | bulk edit, `{task_id}` |
| `deleteDocuments(ids)`, `reprocess(ids)` | moves to trash / re-runs OCR, `{task_id}` |
| `notes(id)`, `addNote(id, text)`, `deleteNote(id, noteId)` | document notes |
| `suggestions(id)` | Paperless' own classifier suggestions as names |
| `similar(id)` | `{count, documents}` with similar content |
| `upload(path, meta)` | uploads a file from `work/` (copy it there first); `meta`: `title`, `created`, `correspondent`, `documentType`, `storagePath`, `tags`, `asn`, `customFields`, `create`; returns a task id |
| `task(id)`, `tasks({status})` | background task state: `status`, `result`, `document` (the new document's id) |
| `download(id, path?, {original})` | saves the archived PDF to `work/paperless/<id>.pdf` (the original to `<id>-original.<ext>`) |
| `thumbnail(id)` | saves `work/paperless/thumbs/<id>.webp`; look at it with `view_image` |
| `shareLink(id, {expiration})` | public link `{url}` |
| `createObject(kind, data)`, `updateObject(kind, ref, changes)`, `deleteObjects(kind, refs)` | manage tags etc.; `kind`: `tag`, `correspondent`, `documentType`, `storagePath`, `customField` |
| `unused(kind)`, `mergeInto(kind, from, into, {deleteSource})` | cleanup: objects without documents; move all documents from one object to another |
| `history(id)`, `stats()` | audit log of a document, archive statistics |
| `get/post/patch/put/delete(path, ...)`, `all(path, params, {limit})` | raw API (JSON, all pages) |

Also exported: `csv(rows, columns?)` (CSV text for `fs.writeText`) and `MATCHING` (matching
algorithm numbers).

```js
// Find, then read the best hit
const { count, documents } = await pl.find({ query: "stromrechnung 2025", correspondent: "Stadtwerke" }, { limit: 5 });
console.log(count, documents.map((d) => `${d.id} ${d.created} ${d.title} ${d.highlights ?? ""}`).join("\n"));
const doc = await pl.doc(documents[0].id, { content: 4000 });

// Classify one document
await pl.update(42, { title: "Stromrechnung März 2025", correspondent: "Stadtwerke", documentType: "Rechnung",
  addTags: ["Strom"], customFields: { Betrag: 84.2 }, removeInboxTags: true });
```

## Rules

- **Ask before destructive or wide changes.** Before deleting documents or objects, merging,
  reprocessing, or changing more than about 10 documents, show the user what will change (a
  count and a few examples) and wait for a yes. Deleted documents go to the trash.
- **Prefer existing vocabulary.** Load `vocabulary()` before classifying, reuse existing
  names, and create new tags, correspondents or types only when the user agrees (`create: true`).
- **Bulk edits run in the background.** `addTags`, `set...`, `deleteDocuments`, `upload` and
  `mergeInto` return a task id; the change shows up seconds later. Check `task(id)` in a later
  call before relying on it.
- **Keep output small.** Print ids, titles and the fields you need; read text with `content`
  limits. Write large results to a file in `work/` and summarize.
- **Answer with sources.** When answering from documents, name the documents (id, title, date)
  you used. If the text looks like bad OCR, say so; `thumbnail(id)` + `view_image` shows the page.
- Never request `/api/profile/`: its response contains the API token.

## More documents (load when needed)

Load one with the `use_skill` tool: `name: "paperless"`, `file: "references/<file>"`.

- `references/search.md`: full-text query syntax, all `find` filters, custom field queries,
  sorting. Load for any search that is more than a few words.
- `references/editing.md`: how fields behave (created date, tags and inbox tags, nested tags,
  custom field types and value formats, ASN, notes, permissions), objects and matching rules.
- `references/recipes.md`: step-by-step playbooks: inbox triage, extracting data into custom
  fields, answering questions and reports, CSV export, cleanup of duplicates and unused objects,
  uploading, sharing.
- `references/automation.md`: lasting automation in Paperless itself: matching rules,
  workflows (triggers and actions), saved views, mail rules.
- `references/rest-api.md`: endpoint reference for raw calls (versions, pagination, fields,
  tasks, document operations such as merge, rotate, split).
