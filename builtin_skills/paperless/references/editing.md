# Document fields and objects

## Document fields

| Field (`update` key) | Notes |
|---|---|
| `title` | Free text. A good title names what the document is, e.g. "Stromrechnung März 2025", without repeating the correspondent or date if those are set. |
| `created` | The document's own date (letter, invoice date), `YYYY-MM-DD`. Not the scan date; that is `added` (read-only). Paperless guesses it from the text, often wrongly (it may pick a birth date or a due date). |
| `correspondent` | Who sent or issued it (company, authority, person). One per document. |
| `documentType` | What kind of document (Rechnung, Vertrag, Bescheid, Kontoauszug, ...). One per document. |
| `storagePath` | Where Paperless stores the file on disk (a path template). Changing it moves the file. |
| `tags` | Any number. `tags` replaces all tags; prefer `addTags` / `removeTags`. |
| `asn` | Archive serial number: the number written on the paper original, unique. `pl.get("/api/documents/next_asn/")` gives the next free one. |
| `customFields` | `{name: value}`, see below. Fields not mentioned stay unchanged; `null` removes a field. |
| `removeInboxTags: true` | Removes all inbox tags: marks the document as processed. |
| `content` (raw key) | The OCR text. Can be corrected with `update(id, { content })`; it is what full-text search uses. |
| `owner`, `set_permissions` (raw keys) | Permissions, see below. |

The document's file cannot be changed through these fields; see `references/rest-api.md` for
rotate, merge, split, new versions.

## Tags

- **Inbox tags** (`is_inbox_tag: true`, `inbox` in `vocabulary()`) are added to every new
  document. A document with an inbox tag is "not processed yet". After classifying it, remove
  them (`removeInboxTags: true`).
- **Nested tags:** a tag can have a `parent`. Adding a child tag also adds its parents;
  removing a parent removes its children. `vocabulary()` shows `parent`.
- Create: `createObject("tag", { name: "Steuer 2025", color: "#a6cee3", parent: "Steuer" })`.

## Custom fields

Create a field once: `createObject("customField", { name: "Betrag", data_type: "monetary" })`.
The type cannot be changed later.

| `data_type` | Value in `customFields` |
|---|---|
| `string` | text, max 128 characters |
| `longtext` | text of any length |
| `url` | a valid URL |
| `date` | `"YYYY-MM-DD"` |
| `boolean` | `true` / `false` |
| `integer` | whole number |
| `float` | number |
| `monetary` | a number (`84.2`, sent with the field's default currency) or a string like `"EUR84.20"`; two decimals, currency is an ISO 4217 code |
| `select` | the option's label (the client converts it to the option id); create with `extra_data: { select_options: [{ label: "offen" }, { label: "bezahlt" }] }` |
| `documentlink` | list of document ids; links are added in both directions |

In results, custom fields appear as `{ "Betrag": "EUR84.20", "Status": "offen" }`.
`setCustomFields(ids, values)` sets the same values on many documents.

## Correspondents, document types, storage paths

`createObject(kind, { name, matching_algorithm, match, is_insensitive })`; storage paths also
need `path`, a template like `{{ correspondent }}/{{ created_year }}/{{ title }}`.
Rename with `updateObject(kind, "Old", { name: "New" })`; all documents follow.

## Matching rules (auto-assignment on consumption)

Each tag, correspondent, document type and storage path has a rule that assigns it to new
documents:

| `matching_algorithm` | `match` |
|---|---|
| `none` | never assigned automatically |
| `any` | any of the words in `match` occurs |
| `all` | all words occur |
| `literal` | the exact text occurs |
| `regex` | the regular expression matches |
| `fuzzy` | approximate match of the text |
| `auto` | a classifier learns from the documents you have tagged (default) |

`is_insensitive: true` ignores case. Pass the algorithm by name:
`updateObject("correspondent", "Stadtwerke", { matching_algorithm: "any", match: "Stadtwerke SWM" })`.
Rules only apply to newly consumed documents (or reprocessed ones). See
`references/automation.md` for workflows.

## Notes

`addNote(id, "Bezahlt am 3.4. per Überweisung")`. Notes are searchable with `notes:` in a
full-text query. Use them for information that is not a field (what was done, why).

## Permissions

Documents and objects have an `owner` and `permissions` (view/change by users and groups).
Read them with `pl.get("/api/documents/<id>/", { full_perms: true })`. Set them with a raw
`patch` (`{ owner: userId, set_permissions: { view: { users: [], groups: [] }, change: { users: [], groups: [] } } }`)
or in bulk with `bulkEdit(ids, "set_permissions", { set_permissions, owner, merge: true })`.
Users and groups: `pl.all("/api/users/")`, `pl.all("/api/groups/")`. The token's user only
sees and changes what it is permitted to; missing documents may be a permissions issue.
