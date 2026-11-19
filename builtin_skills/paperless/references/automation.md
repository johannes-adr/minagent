# Automation inside Paperless

When the user wants something to happen to future documents by itself, set it up in Paperless
instead of doing it by hand each time. Always show the planned rule and ask before creating
or changing it; a wrong rule silently mislabels every new document.

## Matching rules (simplest)

Every tag, correspondent, document type and storage path can assign itself to new documents,
see the table in `references/editing.md`. Prefer `any`/`all`/`literal` with distinctive words
(an IBAN, a customer number, the company name as printed) over `auto` when the user wants
predictable results:

```js
await pl.updateObject("correspondent", "Stadtwerke München", { matching_algorithm: "literal", match: "SWM Versorgungs GmbH", is_insensitive: true });
```

Test a rule before saving it: search the existing documents with the same words
(`find({ text: "SWM Versorgungs GmbH" })`) and check that the hits are right.

## Workflows

Workflows run actions when a trigger fires. API: `pl.all("/api/workflows/")`,
`pl.post("/api/workflows/", workflow)`, `pl.patch("/api/workflows/<id>/", changes)`,
`pl.delete("/api/workflows/<id>/")`. Triggers and actions are nested objects; ids of tags,
correspondents etc. are needed (`await pl.resolve("tag", "Rechnung")`).

```js
await pl.post("/api/workflows/", {
  name: "Stadtwerke-Rechnungen",
  order: 0,
  enabled: true,
  triggers: [{
    type: 2,                         // document added
    sources: [1, 2, 3, 4],           // consume folder, API upload, mail, web UI
    matching_algorithm: 3,           // literal (see MATCHING)
    match: "SWM Versorgungs GmbH",
    is_insensitive: true,
  }],
  actions: [{
    type: 1,                         // assignment
    assign_title: "Stromrechnung {{ created_month_name }} {{ created_year }}",
    assign_correspondent: await pl.resolve("correspondent", "Stadtwerke München"),
    assign_document_type: await pl.resolve("documentType", "Rechnung"),
    assign_tags: [await pl.resolve("tag", "Strom")],
  }],
});
```

**Trigger `type`:** 1 consumption started (before OCR: only file name, path, source and mail
rule are known), 2 document added (after OCR and classification: content matching and all
fields available), 3 document updated, 4 scheduled (relative to a date field), 5 manual.

**Trigger filters:** `sources` (1 consume folder, 2 API upload, 3 mail, 4 web UI),
`filter_filename` and `filter_path` (shell-style globs like `*rechnung*`), `filter_mailrule`,
`matching_algorithm` + `match` + `is_insensitive` (on the content), `filter_has_tags` (any),
`filter_has_all_tags`, `filter_has_not_tags`, `filter_has_correspondent`,
`filter_has_document_type`, `filter_has_storage_path`, `filter_has_any_correspondents`,
`filter_has_not_correspondents`, ... (lists of ids), `filter_custom_field_query` (a custom
field query as JSON text, see `references/search.md`). Scheduled triggers use
`schedule_date_field` (`added`, `created`, `modified`, `custom_field` with
`schedule_date_custom_field`), `schedule_offset_days`, `schedule_is_recurring`,
`schedule_recurring_interval_days`.

**Action `type`:**

| type | Fields |
|---|---|
| 1 assignment | `assign_title` (template), `assign_tags`, `assign_correspondent`, `assign_document_type`, `assign_storage_path`, `assign_custom_fields` (ids), `assign_custom_fields_values` (`{id: value}`), `assign_owner`, `assign_view_users`, `assign_view_groups`, `assign_change_users`, `assign_change_groups` |
| 2 removal | `remove_tags`, `remove_all_tags`, `remove_correspondents`, `remove_all_correspondents`, `remove_document_types`, `remove_storage_paths`, `remove_custom_fields`, `remove_owners`, `remove_all_permissions`, ... |
| 3 email | `email: { subject, body, to, include_document }` |
| 4 webhook | `webhook: { url, use_params, as_json, params, body, headers, include_document }` |
| 5 password removal | `passwords` |
| 6 move to trash | - |
| 7 remote OCR | - |
| 8 apply AI suggestions | `ai_suggestion_fields`, `ai_create_missing`, `ai_overwrite_existing` |

**Title and webhook templates** are Jinja: `{{ correspondent }}`, `{{ document_type }}`,
`{{ owner_username }}`, `{{ original_filename }}`, `{{ added }}`, `{{ added_year }}`,
`{{ added_month }}`, `{{ added_month_name }}`, `{{ custom_fields | get_cf_value("Betrag", "") }}`;
only for added/updated triggers: `{{ created }}`, `{{ created_year }}`, `{{ created_month_name }}`,
`{{ created_day }}`, `{{ doc_url }}`, `{{ doc_id }}`. Localized dates:
`{{ created | localize_date('MMMM', 'de_DE') }}`.

Look at existing workflows first (`pl.all("/api/workflows/")`) to match the user's style and
avoid duplicates. Workflows apply to new events only; to fix existing documents, use bulk
edits.

## Saved views

Saved views are stored filters shown in the web UI sidebar or dashboard:
`pl.all("/api/saved_views/")`. Their `filter_rules` are `[{ rule_type, value }]` with numeric
rule types; to create one, copy and adapt the rules of an existing view, or ask the user to
create it in the web UI from a filtered document list.

## Mail

`pl.all("/api/mail_accounts/")` and `pl.all("/api/mail_rules/")` show how mail is imported
(folders, filters, which attachments, assigned tags/correspondent). Rules can be changed with
`patch`; account passwords are not readable. `pl.all("/api/processed_mail/")` lists processed
mails, useful to check why an attachment did not arrive.
