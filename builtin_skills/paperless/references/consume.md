# A newly consumed document

Usually a webhook starts this: its message holds the id of a document Paperless has just
consumed (look for `document_id`, `doc_url` or a number in the payload). Nobody is there to
answer questions. Your job is to **propose** the classification and get it applied after the
user approved the one writing call.

## 1. Gather context (reads only: no `writes`)

One call, `hosts: [<paperless host>]`, printing only what you need:

```js
import { connect } from "skill:paperless/scripts/paperless.js";
const pl = connect();
const id = 123; // from the webhook payload
const doc = await pl.doc(id, { content: 6000 });
const vocabulary = await pl.vocabulary();
console.log(JSON.stringify({ doc, suggestions: await pl.suggestions(id), similar: (await pl.similar(id)).documents.slice(0, 5) }));
console.log(JSON.stringify(vocabulary));
console.log(fs.exists("paperless-conventions.md") ? fs.readText("paperless-conventions.md") : "(no conventions file)");
```

- `doc.modified` is what you pass as `unchangedSince` later. Keep it.
- `work/paperless-conventions.md` belongs to the user: their title scheme, tag rules, what
  counts as a correspondent. Follow it over your own taste. **Never write it.** If it is
  missing, use the existing documents (`similar`) as the model.
- If the text is empty or garbled, look at the page: `await pl.thumbnail(id)` and `view_image`.
- A document that already has a title, correspondent and tags the user set (no inbox tag)
  needs little: say so and only fill in what is missing.

## 2. Decide

Fields: **title**, **correspondent**, **document type**, **tags**, **created date** (the
document's own date, not a due date or a birth date), **custom fields** (amount, invoice
number, due date, ...), and **related documents**.

- **Prefer existing vocabulary.** Before proposing a new tag, correspondent or document type,
  search the vocabulary for the same thing under another spelling (case, umlauts, "GmbH",
  abbreviations, singular/plural, other language). A new object is the exception; mark it NEW.
- Use `suggestions` and `similar` as hints: documents from the same sender usually share
  correspondent, type, tags and title scheme.
- Related documents (same contract, earlier invoice, the letter this one answers): add them
  as a note ("Gehört zu #12, #40"), or, if a custom field of type `documentlink` exists, set
  it to their ids.
- Do not guess values you cannot read in the text. Leave a field unchanged and say why.

## 3. Show the proposal

Write it in your message as a compact table, so the user sees it with the approval request:

| Field | Before → After |
|---|---|
| Title | `scan_0012` → `Stromrechnung März 2025` |
| Correspondent | none → Stadtwerke |
| Type | none → Rechnung |
| Tags | `Posteingang` → Strom, Steuer 2025 (**NEW**) |
| Created | 2025-03-01 → 2025-03-14 |
| Betrag | none → EUR84.20 |
| Related | none → note: #40 |

Then, directly, the writing call (step 4): make it with `execute_javascript` and
`writes: true`. Do not paste the script into your message and do not ask the user to approve
it in text; the harness asks for the approval itself when you make the call. One or two
sentences on anything uncertain.

## 4. Write everything in ONE call with `writes: true`

Create the missing objects, update the document, remove the inbox tags: all in that one call,
so the user approves exactly this script once. Write only literal values you decided in steps
1-3 (the same ones as in the table): no `find`, `similar`, `suggestions` or other reads to
decide anything here, no loops. Use `unchangedSince`, so that a change approved hours later
does not overwrite edits somebody made in the meantime (it throws, and nothing is written, if
the document changed).

```js
import { connect } from "skill:paperless/scripts/paperless.js";
const pl = connect();
const id = 123;
await pl.update(id, {
  title: "Stromrechnung März 2025",
  correspondent: "Stadtwerke",
  documentType: "Rechnung",
  addTags: ["Strom", "Steuer 2025"],
  created: "2025-03-14",
  customFields: { Betrag: 84.2 },
  removeInboxTags: true,
}, { create: true, unchangedSince: "<doc.modified>" }); // create: true only because of the NEW objects
await pl.addNote(id, "Gehört zu #40");
console.log("done");
```

Need special settings for a new object (a matching rule, a color, a custom field)? Create it
in this same call before `update`, but call `await pl.assertUnchanged(id, "<doc.modified>")`
first. Do not use `create: true` for names that are not marked NEW.

## 5. Verify and report

After the call ran you get its output back as a message; it ends with every request that
changed data and its status. Read the document again (no `writes`) and compare it with the
table. If the call threw (for example "was changed since you read
it"), read the document again, redo the proposal and write once more (this needs approval
again). Finish with a short report: what was set, what was left alone and why.
