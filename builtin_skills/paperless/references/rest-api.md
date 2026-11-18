# Paperless-ngx REST API reference

For anything the client does not wrap, call the API directly with `pl.get(path, params)`,
`pl.post(path, body)`, `pl.patch`, `pl.put`, `pl.delete`, `pl.all(path, params, { limit })`
(follows pagination) or `pl.request(method, path, { params, body, form, saveTo })`. The
client adds the token and `Accept: application/json; version=10`, and throws on HTTP errors
with the response body.

## Basics

- **Spec:** the server publishes its OpenAPI 3 schema at `/api/schema/` (add
  `{ format: "json" }`) and a browsable UI at `/api/schema/view/`. The schema is large: load it
  in a script and print only the path you need, e.g.
  `const s = await pl.get("/api/schema/", { format: "json" }); console.log(JSON.stringify(s.paths["/api/documents/{id}/"].patch, null, 1));`
- **Versions:** API 10 is current (Paperless-ngx 3.x); 9 is still supported. Responses carry
  `X-Api-Version` and `X-Version` headers (`pl.check()` shows them). For 2.x servers use
  `connect({ apiVersion: 9 })`; some calls (`text` search, `/api/documents/delete/`,
  paginated tasks) may then not exist.
- **Lists** are paginated: `{ count, next, previous, results }`, parameters `page`,
  `page_size` (default 25), `ordering`. Most lists accept `name__icontains`, `id__in`, etc.
- **`fields=id,title,tags`** limits which fields `/api/documents/` returns (much smaller
  responses); `truncate_content=true` shortens `content`; `full_perms=true` returns full
  permissions instead of `user_can_change`.
- **Errors:** 400 with a JSON body naming the invalid field; 403 means missing permission;
  404 can also mean "not visible to this user".
- Never request `/api/profile/`: its response contains the API token.

## Endpoints

| Path | Use |
|---|---|
| `/api/documents/` | list and filter documents (filters in `references/search.md`) |
| `/api/documents/<id>/` | GET, PATCH (fields), DELETE (to trash) one document |
| `/api/documents/<id>/download/`, `/preview/`, `/thumb/` | files; `?original=true` for the original; binary, so use `pl.download` / `pl.thumbnail` or `request("GET", path, { saveTo })` |
| `/api/documents/<id>/metadata/` | file metadata: checksums, sizes, original and archive PDF metadata, language |
| `/api/documents/<id>/notes/` | GET notes, POST `{ note }`, DELETE `?id=<note id>` |
| `/api/documents/<id>/suggestions/` | classifier suggestions (ids) |
| `/api/documents/<id>/history/` | audit log |
| `/api/documents/<id>/share_links/` | share links of a document |
| `/api/documents/<id>/email/`, `/api/documents/email/` | POST `{ addresses, subject, message, use_archive_version }` (the second also `documents`): mail documents (needs mail settings on the server) |
| `/api/documents/next_asn/` | next free archive serial number |
| `/api/documents/post_document/` | upload (multipart; `pl.upload`) |
| `/api/documents/bulk_edit/` | `{ documents, method, parameters }`, methods: `set_correspondent`, `set_document_type`, `set_storage_path`, `add_tag`, `remove_tag`, `modify_tags` (`add_tags`, `remove_tags`), `modify_custom_fields` (`add_custom_fields`, `remove_custom_fields`), `set_permissions` (`set_permissions`, `owner`, `merge`) |
| `/api/documents/delete/`, `/reprocess/` | `{ documents }` (reprocess: `remote_ocr`) |
| `/api/documents/rotate/` | `{ documents, degrees }` (multiple of 90) |
| `/api/documents/merge/` | `{ documents, metadata_document_id?, delete_originals?, archive_fallback? }`: one new PDF from several documents |
| `/api/documents/edit_pdf/` | `{ documents: [id], operations: [{ page, rotate?, doc? }], delete_original?, update_document?, include_metadata? }`: reorder, rotate, delete pages (pages not listed) or split (`doc` = index of the output document) |
| `/api/documents/remove_password/` | `{ documents, password, update_document?, delete_original? }` |
| `/api/documents/merge_as_versions/` | `{ documents, root_document_id }`: make documents versions of one |
| `/api/documents/<id>/update_version/` | multipart `document` (+ `version_label`): new file version |
| `/api/documents/bulk_download/` | `{ documents, content: "archive" \| "originals" \| "both" }`: zip (use `saveTo`) |
| `/api/documents/selection_data/` | `{ documents }`: counts of tags/correspondents/... in a selection |
| `/api/search/?query=` | global search across documents, tags, correspondents, saved views, ... (as in the UI's search box) |
| `/api/search/autocomplete/?term=` | word completions from the index |
| `/api/tags/`, `/api/correspondents/`, `/api/document_types/`, `/api/storage_paths/`, `/api/custom_fields/` | objects (CRUD) |
| `/api/bulk_edit_objects/` | `{ objects, object_type, operation: "delete" \| "set_permissions", owner?, permissions?, merge? }` |
| `/api/saved_views/`, `/api/workflows/`, `/api/workflow_triggers/`, `/api/workflow_actions/` | see `references/automation.md` |
| `/api/mail_accounts/`, `/api/mail_rules/`, `/api/processed_mail/` | mail import |
| `/api/share_links/`, `/api/share_link_bundles/` | public links |
| `/api/tasks/` | background tasks: filter `task_id`, `status` (`pending`, `started`, `success`, `failure`, `revoked`), `task_type` (`consume_file`, `bulk_update`, ...), `is_complete`, `acknowledged`; also `/api/tasks/summary/`, `/active/`, `/status_counts/`, POST `/api/tasks/acknowledge/` `{ tasks: [ids] }` |
| `/api/trash/` | GET deleted documents; POST `{ documents, action: "restore" \| "empty" }` |
| `/api/statistics/` | totals, inbox count, file types, character count |
| `/api/status/` | system health (database, redis, celery, index, classifier); admin only |
| `/api/users/`, `/api/groups/` | users and groups (ids for permissions) |
| `/api/ui_settings/` | the current user and UI settings |
| `/api/logs/`, `/api/logs/<name>/` | server logs (admin) |
| `/api/config/` | application settings such as OCR options (admin) |
| `/api/remote_version/` | newest released Paperless version |

Document operations that change files (rotate, merge, edit_pdf, remove_password,
reprocess) run as tasks and create or reprocess documents; always confirm with the user first.
