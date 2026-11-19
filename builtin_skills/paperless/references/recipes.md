# Recipes

Each recipe is a sequence of `execute_javascript` calls. Start every script with
`import { connect } from "skill:paperless/scripts/paperless.js"; const pl = connect();`.

## Inbox triage (classify new documents)

1. Overview: `const v = await pl.vocabulary();` and `await pl.find({ inbox: true }, { limit: 50 })`.
   Keep the vocabulary in mind (or save it to `work/paperless-vocabulary.json`).
2. For each document (or a batch of 5-10 per call): `await pl.doc(id, { content: 3000 })`,
   optionally `await pl.suggestions(id)` (Paperless' classifier). If the text is empty or
   garbled, look at `await pl.thumbnail(id)` with `view_image`.
3. Decide: title, created date (the document's own date, not a due date), correspondent,
   document type, tags, custom fields (amount, invoice number, due date ...). Use existing
   names; collect proposed new ones.
4. Show the user a table of the proposals (id, current title → new values), mark new objects,
   and ask for approval. Small, obvious cases may be applied directly if the user said so.
5. Apply: `await pl.update(id, { ..., removeInboxTags: true })` for each document. Pass
   `{ create: true }` only for approved new names.
6. Report what changed. Suggest matching rules (`references/automation.md`) for recurring
   correspondents so Paperless does it by itself next time.

## Extract data into custom fields

Example: amount and invoice number of all invoices without them.

1. Make sure the fields exist (`vocabulary().customFields`); create them if the user agrees.
2. `const { documents } = await pl.find({ documentType: "Rechnung", customFields: ["Betrag", "exists", false] }, { limit: Infinity, content: 4000 });`
3. Extract from `content` with regular expressions where the format is regular (one
   correspondent's invoices look alike); read the text yourself where it is not. Note
   uncertain values instead of guessing.
4. Show a sample, then write: `await pl.update(id, { customFields: { Betrag: 84.2, Rechnungsnummer: "R-2025-0815" } })`.

## Answer a question from the archive

"When does my car insurance end?", "How much did I pay for electricity in 2024?"

1. Search with full-text and filters (`references/search.md`); try 2-3 query variants.
2. Read the most relevant documents with `doc(id, { content })`; limit characters, read more if needed.
3. Answer with the facts and cite the documents: `#id title (created)`. Sum amounts in the
   script, not in your head, and say which documents were included or missing.

## Reports and exports

```js
import { connect, csv } from "skill:paperless/scripts/paperless.js";
const pl = connect();
const { documents } = await pl.find({ documentType: "Rechnung", createdFrom: "2025-01-01", createdTo: "2025-12-31" }, { limit: Infinity });
const rows = documents.map((d) => ({ id: d.id, date: d.created, from: d.correspondent, title: d.title, amount: d.custom_fields?.Betrag ?? "" }));
fs.writeText("reports/rechnungen-2025.csv", csv(rows));
console.log(rows.length, "rows written to work/reports/rechnungen-2025.csv");
```

For tax collections ("everything for my 2025 tax return"): search by tags/types and
full-text, list candidates for the user to confirm, then tag them (`addTags(ids, "Steuer 2025")`)
and optionally download them (`download(id)` into `work/paperless/`).

## Cleanup

- **Unused objects:** `await pl.unused("tag")` (also correspondent, documentType,
  storagePath). Show the list, delete approved ones with `deleteObjects(kind, names)`.
- **Duplicate objects** ("Telekom", "Telekom Deutschland GmbH", "telekom"): list
  `vocabulary()` names, group near-duplicates, propose the name to keep, then after approval
  `await pl.mergeInto("correspondent", "telekom", "Telekom Deutschland GmbH", { deleteSource: true })`
  per duplicate.
- **Duplicate documents:** `find({ duplicates: true })` finds identical files;
  `similar(id)` finds near-duplicates by content. Compare `created`, `pages` and content,
  propose which to delete; `deleteDocuments(ids)` moves them to the trash.
- **Missing metadata:** `find({ correspondent: null })`, `find({ documentType: null })`,
  `find({ tagged: false })`, then classify like the inbox triage.
- **Bad titles** (file names like `scan_0042`): `find({ title: "scan" })`, read the content,
  propose titles, apply after approval.
- **Bad OCR:** `reprocess(ids)` re-runs OCR (takes a while; check `tasks()` later).

## Upload a file

1. Copy the file into the work directory, e.g. with `execute`: `cp ~/Downloads/brief.pdf work/upload/`.
2. `const taskId = await pl.upload("upload/brief.pdf", { title: "...", correspondent: "...", tags: ["..."] });`
3. In a later call: `await pl.task(taskId)` → `status: "success"` and `document` (the new id),
   or `"failure"` with `result` (e.g. a duplicate). Then classify the new document if needed.

## Share a document

`await pl.shareLink(id, { expiration: "2026-12-31T00:00:00Z" })` returns a public URL that
works without login; tell the user it is public until it expires. To send the file instead,
`download(id)` saves it to `work/paperless/`.
