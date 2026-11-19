# Searching Paperless-ngx

There are two ways to find documents, and `find(filters)` combines both:

- **Full-text search** (`query`): scored by relevance, searches title, content,
  correspondent, type and tags, returns `score` and `highlights` (matches in `**bold**`).
  Use it for "the letter about X".
- **Filters** (everything else): exact database conditions (tag, date range, custom field
  value, ...), sorted by `sort`. Use them for "all invoices from 2024".

Both together: `find({ query: "kündigung", correspondent: "Telekom", createdFrom: "2023-01-01" })`.

## `find` filters

| Filter | Meaning |
|---|---|
| `query: "..."` | full-text query, syntax below |
| `text: "..."` | simple search for the words in title or content (no syntax) |
| `title: "..."`, `content: "..."` | title / content contains the text (case-insensitive) |
| `moreLike: id` | documents similar to document `id` |
| `tags: [a, b]` | has all these tags |
| `anyTags: [a, b]` | has at least one of them |
| `notTags: [a, b]` | has none of them |
| `tagged: false` | has no tags at all |
| `inbox: true` | has an inbox tag (not yet processed) |
| `correspondent: x` | this correspondent; `[x, y]` any of them; `null` none set |
| `documentType: x`, `storagePath: x` | the same for document type and storage path |
| `notCorrespondent: [x]`, `notDocumentType: [x]` | exclude these |
| `createdFrom: "2024-01-01"`, `createdTo: "2024-12-31"` | document date range (inclusive) |
| `addedFrom`, `addedTo`, `modifiedFrom` | when it was added / last changed (`YYYY-MM-DD`) |
| `asn: 123`, `hasAsn: false` | archive serial number |
| `customFields: [...]` | custom field query, see below |
| `hasCustomField: [name]` | the field is set on the document (any value) |
| `duplicates: true` | documents whose file also exists as another document |
| `mimeType: "application/pdf"` | file type |
| `ids: [1, 2]` | these documents |
| `sort: "-created"` | order: `created`, `added`, `modified`, `title`, `correspondent__name`, `document_type__name`, `archive_serial_number`, `page_count`, `num_notes`, `custom_field_<id>`; `-` = descending. Full-text results are sorted by score unless `sort` is given |

Any other key is passed to `/api/documents/` unchanged, e.g. `original_file_name__icontains`,
`owner__id`, `title__istartswith`, `checksum__iexact`.

`find` returns `{count, documents}`: `count` is the total on the server, `documents` at most
`limit` (default 25). Use `count(filters)` when only the number matters and `ids(filters)`
before bulk edits.

## Full-text query syntax (`query`)

- All words must match, in any order. Matching ignores case, accents and separators (`1312`
  finds `A-1312/B`). Words are stemmed for the configured language.
- `AND`, `OR`, `NOT` in capitals, parentheses group: `versicherung AND (kfz OR auto) NOT angebot`.
  A leading `-` does **not** exclude.
- `"exact phrase"` matches the words in that order.
- Wildcards: `rechn*` (any characters), `20?4` (one character). If a wildcard finds nothing,
  shorten the prefix (stemming).
- Fields: `title:`, `content:`, `correspondent:`, `type:` (document type), `path:` (storage
  path), `tag:`, `original_filename:`, `asn:`, `page_count:`, `num_notes:`, `checksum:`,
  `created:`, `added:`, `modified:`. Quote multi-word values: `correspondent:"deutsche bahn"`.
  `tag:steuer,2024` requires both tags.
- Numbers take ranges: `asn:[100 to 200]`.
- Dates: `created:2024`, `created:2024-03`, `created:2024-03-15`, `added:yesterday`,
  `created:"previous year"` (also `today`, `this month`, `previous month`, `previous quarter`,
  `this year`, ...), ranges `created:[2020 to 2022]`, `added:[-1 week to now]`. Relative times
  (`-1 week`, `now`) only work as range bounds.
- Custom fields and notes are only searched with their prefix:
  `custom_fields.value:4711`, `custom_fields.name:"Vertragsnummer" custom_fields.value:4711`,
  `notes:rückruf`, `notes.user:anna`.

Examples:

```js
await pl.find({ query: 'type:rechnung created:"previous year" (strom OR gas)' }, { limit: 50 });
await pl.find({ query: '"fristlose kündigung"' }, { content: 2000 });
await pl.find({ query: "custom_fields.value:4711" });
```

## Custom field queries (`customFields`)

A JSON expression `[field, operator, value]`, where `field` is the field's name or id;
combine with `["AND", [...]]` / `["OR", [...]]` and negate with `["NOT", expr]`.

| Field types | Operators |
|---|---|
| all | `exact`, `in` (list), `isnull` (true/false), `exists` (true/false) |
| text, URL, monetary | `icontains`, `istartswith`, `iendswith` |
| integer, number, date, monetary | `gt`, `gte`, `lt`, `lte`, `range` (`[from, to]`) |
| document link | `contains` (list of document ids, all must be linked) |
| select | `exact`, `in` with option labels |

Dates also take relative expressions: `["Fällig", "range", ["today", "in 30 days"]]`,
`["Ablauf", "lt", "1 month ago"]`.

```js
// Unpaid invoices over 100 EUR due in the next 30 days
await pl.find({ customFields: ["AND", [
  ["Betrag", "gte", 100],
  ["Bezahlt", "exact", false],
  ["Fällig", "range", ["today", "in 30 days"]],
]] });
// Documents where the field is missing or empty
await pl.find({ customFields: ["OR", [["Rechnungsnummer", "exists", false], ["Rechnungsnummer", "exact", ""]]] });
```

## Tips

- Search in the document's language (German archive → German words). Try synonyms and
  shorter stems if there are no hits; check spelling variants of names (OCR errors).
- Narrow broad full-text results with filters instead of more words.
- To scan many documents for something a query cannot express, page through
  `find(filters, { limit: Infinity, content: 3000 })` in one script and test the text in
  JavaScript; print only the matches.
- `inbox: true` lists what still needs processing; `tagged: false` or `correspondent: null`
  finds incomplete metadata.
