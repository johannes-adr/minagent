// Paperless-ngx client for the execute_javascript tool (not Node): only `fetch`, `fs` and
// `console` exist. Written for Paperless-ngx 3.x (REST API version 10).
//
//   import { connect } from "skill:paperless/scripts/paperless.js";
//   const pl = connect();                       // URL from work/paperless.json
//   const { count, documents } = await pl.find({ query: "stromrechnung", createdFrom: "2025-01-01" });
//
// Documents come back compact: names instead of ids, custom fields as {name: value}, no
// content unless asked for. Objects (tags, correspondents, ...) are referenced by name or id.

const CONFIG = "paperless.json";
const DEFAULT_SECRET = "PAPERLESS";
const API_VERSION = 10;
const DEFAULT_CONTENT_CHARS = 20000;

/** The object types and their API collection names. */
const KINDS = {
  tag: "tags",
  tags: "tags",
  correspondent: "correspondents",
  correspondents: "correspondents",
  documentType: "document_types",
  document_type: "document_types",
  document_types: "document_types",
  type: "document_types",
  storagePath: "storage_paths",
  storage_path: "storage_paths",
  storage_paths: "storage_paths",
  customField: "custom_fields",
  custom_field: "custom_fields",
  custom_fields: "custom_fields",
};

/** Matching algorithms of tags, correspondents, document types and storage paths. */
export const MATCHING = { none: 0, any: 1, all: 2, literal: 3, regex: 4, fuzzy: 5, auto: 6 };
const MATCHING_NAMES = Object.fromEntries(Object.entries(MATCHING).map(([k, v]) => [v, k]));

/** Document fields fetched when no content is requested (keeps responses small). */
const LIST_FIELDS = [
  "id", "title", "created", "added", "modified", "correspondent", "document_type", "storage_path",
  "tags", "archive_serial_number", "custom_fields", "page_count", "original_file_name", "mime_type",
];

function collection(kind) {
  const name = KINDS[kind];
  if (!name) throw new Error(`unknown object kind ${JSON.stringify(kind)}; use one of: tag, correspondent, documentType, storagePath, customField`);
  return name;
}

function list(value) {
  if (value == null) return [];
  return Array.isArray(value) ? value : [value];
}

/** Levenshtein distance, for "did you mean" hints. */
function distance(a, b) {
  const row = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    let prev = row[0];
    row[0] = i;
    for (let j = 1; j <= b.length; j++) {
      const tmp = row[j];
      row[j] = Math.min(row[j] + 1, row[j - 1] + 1, prev + (a[i - 1] === b[j - 1] ? 0 : 1));
      prev = tmp;
    }
  }
  return row[b.length];
}

/** A name as it is compared: no case, accents, punctuation or repeated spaces. */
function fold(name) {
  return String(name)
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "")
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, " ")
    .trim();
}

/** Up to `n` names of `objects` that look like `name` (substring or few typos). */
function similarNames(name, objects, n = 5) {
  const needle = name.toLowerCase();
  const tolerance = Math.max(2, Math.floor(needle.length / 3));
  return objects
    .map((o) => {
      const hay = o.name.toLowerCase();
      const score = hay.includes(needle) || needle.includes(hay) ? 0 : distance(needle, hay);
      return [score, o.name];
    })
    .filter(([score]) => score <= tolerance)
    .sort((a, b) => a[0] - b[0])
    .slice(0, n)
    .map(([, name]) => name);
}

/**
 * Groups `names` that may mean the same thing: equal apart from case, accents and
 * punctuation, one containing the other ("Telekom", "Telekom Deutschland GmbH"), or a few
 * letters apart ("Versicherung", "Versicherungen"). Returns only groups with more than one
 * spelling, as arrays of the given names; whether they really are the same is your decision
 * (synonyms such as "Auto" and "KFZ" are not found).
 */
export function similarGroups(names) {
  const unique = [...new Set(names.map((n) => String(n).trim()).filter(Boolean))];
  const folded = unique.map(fold);
  const parent = unique.map((_, i) => i);
  const root = (i) => (parent[i] === i ? i : (parent[i] = root(parent[i])));
  const numbers = (s) => (s.match(/\d+/g) ?? []).join(" ");
  const alike = (a, b) => {
    if (a === b) return true;
    if (numbers(a) !== numbers(b)) return false; // "Steuer 2024", "Steuer 2025"
    const shorter = Math.min(a.length, b.length);
    if (shorter >= 4 && (a.includes(b) || b.includes(a))) return true;
    return distance(a, b) <= Math.max(1, Math.floor(shorter / 4));
  };
  for (let i = 0; i < unique.length; i++) {
    for (let j = i + 1; j < unique.length; j++) {
      if (alike(folded[i], folded[j])) parent[root(j)] = root(i);
    }
  }
  const groups = new Map();
  unique.forEach((name, i) => groups.set(root(i), [...(groups.get(root(i)) ?? []), name]));
  return [...groups.values()].filter((g) => g.length > 1);
}

function stripHighlights(html) {
  if (!html) return undefined;
  return html
    .replace(/<(b|strong|span class="match")>(.*?)<\/(b|strong|span)>/gs, "**$2**")
    .replace(/<[^>]+>/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

function truncate(text, max) {
  if (text == null || max === true || max == null || text.length <= max) return text;
  return `${text.slice(0, max)}\n…[truncated: ${text.length} characters in total; pass a larger \`content\` to read more]`;
}

/** Rows (objects) as CSV text; `columns` defaults to the keys of the first row. */
export function csv(rows, columns) {
  columns = columns ?? Object.keys(rows[0] ?? {});
  const cell = (v) => {
    if (v == null) return "";
    const s = Array.isArray(v) ? v.join(", ") : typeof v === "object" ? JSON.stringify(v) : String(v);
    return /[",\n\r;]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  return [columns.join(","), ...rows.map((r) => columns.map((c) => cell(r[c])).join(","))].join("\n") + "\n";
}

function compactTask(task) {
  return {
    task_id: task.task_id,
    status: task.status,
    type: task.task_type ?? task.task_name,
    file: task.task_file_name ?? task.input_data?.filename ?? undefined,
    result: task.result ?? task.result_message ?? undefined,
    document: task.related_document ?? task.result_data?.document_id ?? undefined,
    created: task.date_created,
    done: task.date_done ?? undefined,
  };
}

export class PaperlessError extends Error {
  constructor(method, path, status, body) {
    let hint = "";
    if (status === 401 || status === 403) {
      hint = " (check the token: AGENT_SECRET_PAPERLESS=<host> Token <api token>, and the user's permissions)";
    } else if (status === 406) {
      hint = ` (the server does not support API version ${API_VERSION}; pass connect({ apiVersion: 9 }) for Paperless-ngx 2.x)`;
    }
    super(`Paperless ${method} ${path} failed with ${status}${hint}: ${body.slice(0, 2000)}`);
    this.status = status;
    this.body = body;
  }
}

/**
 * Connects to Paperless-ngx. `url` defaults to `work/paperless.json` ({"url": "..."});
 * `secret` names the configured secret (default `PAPERLESS`).
 */
export function connect(options = {}) {
  let url = options.url;
  if (!url && fs.exists(CONFIG)) url = JSON.parse(fs.readText(CONFIG)).url;
  if (!url) {
    throw new Error(
      `Paperless URL unknown. Ask the user for it (exactly as they open Paperless in the browser), ` +
        `then save it with saveUrl(url), which checks the connection first.`,
    );
  }
  return new Paperless({ ...options, url });
}

/**
 * Saves the Paperless URL the user gave to work/paperless.json, after checking that it
 * connects. An existing, different URL is only replaced with `{ replace: true }`, which needs
 * the user's explicit request. Returns the result of check().
 */
export async function saveUrl(url, { replace = false, ...options } = {}) {
  url = String(url).trim().replace(/\/+$/, "");
  const saved = fs.exists(CONFIG) ? JSON.parse(fs.readText(CONFIG)).url : null;
  if (saved && saved !== url && !replace) {
    throw new Error(
      `${CONFIG} already has ${saved}. Do not change it on your own; only if the user asks for ` +
        `the new URL, call saveUrl(url, { replace: true }).`,
    );
  }
  const check = await new Paperless({ ...options, url }).check();
  if (!check.ok) throw new Error(`not saved, the connection failed: ${check.error}`);
  fs.writeText(CONFIG, JSON.stringify({ url }));
  return check;
}

export class Paperless {
  constructor({ url, secret = DEFAULT_SECRET, apiVersion = API_VERSION }) {
    this.base = String(url).replace(/\/+$/, "");
    this.secret = secret;
    this.apiVersion = apiVersion;
    this.cache = {};
  }

  /** The host to pass in execute_javascript's `hosts`. */
  get host() {
    return this.base.replace(/^[a-z]+:\/\//, "").replace(/[/:].*$/, "");
  }

  url(path, params = {}) {
    const query = Object.entries(params)
      .filter(([, v]) => v !== undefined && v !== null && v !== "")
      .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(Array.isArray(v) ? v.join(",") : v)}`)
      .join("&");
    const full = path.startsWith("http") ? path : `${this.base}${path.startsWith("/") ? "" : "/"}${path}`;
    return query ? `${full}${full.includes("?") ? "&" : "?"}${query}` : full;
  }

  /** Raw request. Returns parsed JSON (or text); with `saveTo` returns {path, bytes, type}. */
  async request(method, path, { params, body, form, saveTo } = {}) {
    if (/\/api\/profile\/?(\?|$)/.test(path)) {
      throw new Error("/api/profile/ returns the API token, so it is not requested; check() tells the user");
    }
    const headers = { Accept: `application/json; version=${this.apiVersion}` };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    let res;
    try {
      res = await fetch(this.url(path, params), {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        form,
        saveTo,
        secret: this.secret,
      });
    } catch (e) {
      if (/unknown secret/.test(e.message)) {
        throw new Error(`${e.message}: configure AGENT_SECRET_${this.secret}=${this.host} Token <api token> in .env and restart the agent`);
      }
      if (/not declared/.test(e.message)) {
        throw new Error(`${e.message}: hosts: ["${this.host}"]`);
      }
      throw e;
    }
    if (!res.ok) throw new PaperlessError(method, path, res.status, await res.text());
    if (saveTo) return { path: saveTo, bytes: res.saved, type: res.headers["content-type"] };
    const text = await res.text();
    if (!text) return null;
    try {
      return JSON.parse(text);
    } catch (_) {
      return text;
    }
  }

  get(path, params) {
    return this.request("GET", path, { params });
  }
  post(path, body) {
    return this.request("POST", path, { body });
  }
  patch(path, body) {
    return this.request("PATCH", path, { body });
  }
  put(path, body) {
    return this.request("PUT", path, { body });
  }
  delete(path, params) {
    return this.request("DELETE", path, { params });
  }

  /**
   * All results of a paginated list endpoint, up to `limit`. Returns the results array; its
   * `count` property is the total number of matches on the server.
   */
  async all(path, params = {}, { limit = Infinity, pageSize = 100 } = {}) {
    const size = Math.max(1, Math.min(pageSize, limit));
    const out = [];
    let count = 0;
    for (let page = 1; out.length < limit; page++) {
      const data = await this.get(path, { ...params, page, page_size: size });
      if (Array.isArray(data)) {
        out.push(...data);
        count = data.length;
        break;
      }
      count = data.count;
      out.push(...data.results);
      if (!data.next || data.results.length === 0) break;
    }
    const results = out.slice(0, limit);
    results.count = count;
    return results;
  }

  /** Who and what we are connected to: {url, ok, user, serverVersion, apiVersion}. */
  async check() {
    let res;
    // Not /api/profile/: its response contains the API token.
    try {
      res = await fetch(this.url("/api/ui_settings/"), {
        headers: { Accept: `application/json; version=${this.apiVersion}` },
        secret: this.secret,
      });
    } catch (e) {
      return { url: this.base, ok: false, error: e.message, hosts: [this.host] };
    }
    if (!res.ok) {
      return { url: this.base, ok: false, error: new PaperlessError("GET", "/api/ui_settings/", res.status, await res.text()).message };
    }
    const settings = await res.json();
    return {
      url: this.base,
      ok: true,
      user: settings.user?.username,
      superuser: settings.user?.is_superuser,
      serverVersion: res.headers["x-version"],
      apiVersion: res.headers["x-api-version"],
    };
  }

  // ---------------------------------------------------------------- objects

  /** All objects of a kind (tag, correspondent, documentType, storagePath, customField), cached per connection. */
  async objects(kind) {
    const name = collection(kind);
    if (!this.cache[name]) this.cache[name] = await this.all(`/api/${name}/`, {}, { pageSize: 500 });
    return this.cache[name];
  }

  /** Forgets the cached objects (after creating or renaming outside this client). */
  refresh() {
    this.cache = {};
  }

  /**
   * The vocabulary for classifying documents, compact: tags (with `inbox` and `parent`),
   * correspondents, document types, storage paths, custom fields (with type and options),
   * each with its document count and matching rule.
   */
  async vocabulary() {
    // An empty `match` never matches, except for the learned `auto` rule.
    const matching = (o) =>
      o.matching_algorithm === MATCHING.auto
        ? "auto"
        : o.matching_algorithm === MATCHING.none || !o.match
          ? undefined
          : `${MATCHING_NAMES[o.matching_algorithm]}: ${o.match}${o.is_insensitive ? " (any case)" : ""}`;
    const tagNames = Object.fromEntries((await this.objects("tag")).map((t) => [t.id, t.name]));
    return {
      tags: (await this.objects("tag")).map((t) => ({
        id: t.id,
        name: t.name,
        documents: t.document_count ?? 0,
        inbox: t.is_inbox_tag || undefined,
        parent: t.parent ? tagNames[t.parent] ?? t.parent : undefined,
        matching: matching(t),
      })),
      correspondents: (await this.objects("correspondent")).map((c) => ({
        id: c.id,
        name: c.name,
        documents: c.document_count ?? 0,
        last: c.last_correspondence ?? undefined,
        matching: matching(c),
      })),
      documentTypes: (await this.objects("documentType")).map((t) => ({
        id: t.id,
        name: t.name,
        documents: t.document_count ?? 0,
        matching: matching(t),
      })),
      storagePaths: (await this.objects("storagePath")).map((p) => ({
        id: p.id,
        name: p.name,
        path: p.path,
        documents: p.document_count ?? 0,
        matching: matching(p),
      })),
      customFields: (await this.objects("customField")).map((f) => ({
        id: f.id,
        name: f.name,
        type: f.data_type,
        options: f.data_type === "select" ? (f.extra_data?.select_options ?? []).map((o) => o.label) : undefined,
        currency: f.data_type === "monetary" ? f.extra_data?.default_currency ?? undefined : undefined,
        documents: f.document_count ?? 0,
      })),
    };
  }

  /**
   * The vocabulary as short lines (`id:name (documents)`, inbox tags marked `*`), for reading
   * it in a script's output without much text. Same data as `vocabulary()`.
   */
  async vocabularyText() {
    const line = (title, objects, label = (o) => o.name) =>
      `${title}: ${objects.map((o) => `${o.id}:${label(o)} (${o.document_count ?? 0})`).join(", ") || "none"}`;
    const tags = await this.objects("tag");
    const byId = Object.fromEntries(tags.map((t) => [t.id, t.name]));
    return [
      line("tags", tags, (t) => (t.parent ? `${byId[t.parent] ?? t.parent}/` : "") + t.name + (t.is_inbox_tag ? "*" : "")),
      line("correspondents", await this.objects("correspondent")),
      line("document types", await this.objects("documentType")),
      line("storage paths", await this.objects("storagePath")),
      `custom fields: ${(await this.objects("customField")).map((f) => `${f.id}:${f.name} [${f.data_type}]`).join(", ") || "none"}`,
    ].join("\n");
  }

  /**
   * Whether the names you want to use exist, decided here and not by you reading a list.
   * `await pl.check({ tags: ["Steuer 2026"], correspondent: "Stadtwerke", documentType: "Rechnung" })`
   * (also `storagePath`, `customFields`) returns one line per name: `exists as "<name>"` (write
   * exactly that name; it can differ from yours in case, accents or punctuation) or `NEW`,
   * with the most similar existing names. Only call a thing NEW when this says so.
   */
  async check(proposal) {
    const kinds = { tags: "tag", correspondent: "correspondent", documentType: "documentType", storagePath: "storagePath", customFields: "customField" };
    const lines = [];
    for (const [key, kind] of Object.entries(kinds)) {
      for (const name of list(proposal[key])) {
        const objects = await this.objects(kind);
        const wanted = fold(name);
        const found = objects.find((o) => fold(o.name) === wanted);
        if (found) {
          lines.push(`${kind} ${JSON.stringify(name)}: exists as ${JSON.stringify(found.name)} (id ${found.id})`);
        } else {
          const similar = similarNames(String(name), objects);
          lines.push(`${kind} ${JSON.stringify(name)}: NEW` + (similar.length ? `; similar existing: ${similar.map((n) => JSON.stringify(n)).join(", ")}` : ""));
        }
      }
    }
    return lines.join("\n") || "(nothing to check)";
  }

  /** The object of `kind` named (ignoring case, accents and punctuation) or numbered `ref`, or undefined. */
  async findObject(kind, ref) {
    const objects = await this.objects(kind);
    if (typeof ref === "number") return objects.find((o) => o.id === ref);
    const name = String(ref).trim().toLowerCase();
    // The exact name (any case) first, then the same name apart from accents and punctuation.
    return objects.find((o) => o.name.toLowerCase() === name) ?? objects.find((o) => fold(o.name) === fold(ref));
  }

  /**
   * The id of the object of `kind` named or numbered `ref`. Numbers are ids, strings are names.
   * With `create: true`, a missing name is created. Throws with similar names otherwise.
   */
  async resolve(kind, ref, { create = false } = {}) {
    if (ref == null) return null;
    if (typeof ref === "object" && ref.id != null) return ref.id;
    const found = await this.findObject(kind, ref);
    if (found) return found.id;
    if (typeof ref === "number") throw new Error(`no ${kind} with id ${ref}`);
    if (create) return (await this.createObject(kind, { name: String(ref).trim() })).id;
    const similar = similarNames(String(ref), await this.objects(kind));
    throw new Error(
      `no ${kind} named ${JSON.stringify(ref)}` +
        (similar.length ? `; similar: ${similar.map((s) => JSON.stringify(s)).join(", ")}` : "; vocabulary() lists all") +
        ` (pass { create: true } where supported to create it)`,
    );
  }

  async resolveAll(kind, refs, options) {
    const ids = [];
    for (const ref of list(refs)) ids.push(await this.resolve(kind, ref, options));
    return ids;
  }

  /** Creates a tag, correspondent, document type, storage path or custom field. */
  async createObject(kind, data) {
    const body = { ...data };
    if (typeof body.matching_algorithm === "string") body.matching_algorithm = MATCHING[body.matching_algorithm];
    if (body.parent != null && collection(kind) === "tags") body.parent = await this.resolve("tag", body.parent);
    const created = await this.post(`/api/${collection(kind)}/`, body);
    if (this.cache[collection(kind)]) this.cache[collection(kind)].push(created);
    return created;
  }

  /** Changes an object (rename, color, matching rule, ...); `ref` is its name or id. */
  async updateObject(kind, ref, changes) {
    const id = await this.resolve(kind, ref);
    const body = { ...changes };
    if (typeof body.matching_algorithm === "string") body.matching_algorithm = MATCHING[body.matching_algorithm];
    if (body.parent != null && collection(kind) === "tags") body.parent = await this.resolve("tag", body.parent);
    const updated = await this.patch(`/api/${collection(kind)}/${id}/`, body);
    delete this.cache[collection(kind)];
    return updated;
  }

  /** Deletes objects by name or id (documents keep existing, they just lose the object). */
  async deleteObjects(kind, refs) {
    const ids = await this.resolveAll(kind, refs);
    if (!ids.length) return null;
    const result = await this.post("/api/bulk_edit_objects/", {
      objects: ids,
      object_type: collection(kind),
      operation: "delete",
    });
    delete this.cache[collection(kind)];
    return result;
  }

  /** Objects of `kind` that no document uses. */
  async unused(kind) {
    return (await this.objects(kind)).filter((o) => o.document_count === 0).map((o) => ({ id: o.id, name: o.name }));
  }

  /**
   * Moves all documents from object `from` to object `into` (tags, correspondents, document
   * types, storage paths), e.g. to merge duplicates. The move runs as a background task. With
   * `deleteSource: true`, `from` is deleted right after queuing it.
   */
  async mergeInto(kind, from, into, { deleteSource = false } = {}) {
    const name = collection(kind);
    const fromId = await this.resolve(kind, from);
    const intoId = await this.resolve(kind, into);
    if (fromId === intoId) throw new Error("`from` and `into` are the same object");
    if (name === "tags") {
      // Nested tags: a child implies its parents, so merging along one branch changes nothing.
      const tags = await this.objects("tag");
      const ancestors = (id) => {
        const out = [];
        for (let t = tags.find((t) => t.id === id); t?.parent; t = tags.find((x) => x.id === t.parent)) out.push(t.parent);
        return out;
      };
      if (ancestors(intoId).includes(fromId) || ancestors(fromId).includes(intoId)) {
        throw new Error("one tag is a parent of the other; with nested tags, retag the documents with addTags/removeTags instead");
      }
    }
    const filter = { tags: "tags__id__all", correspondents: "correspondent__id", document_types: "document_type__id", storage_paths: "storage_path__id" }[name];
    if (!filter) throw new Error(`mergeInto does not support ${kind}`);
    const ids = (await this.all("/api/documents/", { [filter]: fromId, fields: "id" }, { pageSize: 1000 })).map((d) => d.id);
    let task = null;
    if (ids.length) {
      task =
        name === "tags"
          ? await this.bulkEdit(ids, "modify_tags", { add_tags: [intoId], remove_tags: [fromId] })
          : await this.bulkEdit(ids, `set_${name.replace(/s$/, "")}`, { [name.replace(/s$/, "")]: intoId });
    }
    if (deleteSource) await this.deleteObjects(kind, [fromId]);
    return { documents: ids.length, task: task?.task_id ?? null, deleted: deleteSource };
  }

  // -------------------------------------------------------------- documents

  /** API query parameters for friendly `filters` (unknown keys pass through unchanged). */
  async params(filters = {}) {
    const p = {};
    const ids = (kind, refs) => this.resolveAll(kind, refs);
    for (const [key, value] of Object.entries(filters)) {
      if (value === undefined) continue;
      switch (key) {
        case "query": p.query = value; break; // full-text search syntax
        case "text": p.text = value; break; // substring in title or content
        case "title": p.title__icontains = value; break;
        case "content": p.content__icontains = value; break;
        case "ids": p.id__in = list(value).join(","); break;
        case "tags": p.tags__id__all = (await ids("tag", value)).join(","); break;
        case "anyTags": p.tags__id__in = (await ids("tag", value)).join(","); break;
        case "notTags": p.tags__id__none = (await ids("tag", value)).join(","); break;
        case "tagged": p.is_tagged = value; break;
        case "inbox": p.is_in_inbox = value; break;
        case "correspondent":
        case "documentType":
        case "storagePath": {
          const field = { correspondent: "correspondent", documentType: "document_type", storagePath: "storage_path" }[key];
          if (value === null) p[`${field}__isnull`] = true;
          else if (Array.isArray(value)) p[`${field}__id__in`] = (await ids(key, value)).join(",");
          else p[`${field}__id`] = await this.resolve(key, value);
          break;
        }
        case "notCorrespondent": p.correspondent__id__none = (await ids("correspondent", value)).join(","); break;
        case "notDocumentType": p.document_type__id__none = (await ids("documentType", value)).join(","); break;
        case "createdFrom": p.created__gte = value; break;
        case "createdTo": p.created__lte = value; break;
        case "addedFrom": p.added__date__gte = value; break;
        case "addedTo": p.added__date__lte = value; break;
        case "modifiedFrom": p.modified__date__gte = value; break;
        case "asn": p.archive_serial_number = value; break;
        case "hasAsn": p.archive_serial_number__isnull = !value; break;
        case "customFields": p.custom_field_query = typeof value === "string" ? value : JSON.stringify(value); break;
        case "hasCustomField": p.custom_fields__id__all = (await ids("customField", value)).join(","); break;
        case "moreLike": p.more_like_id = value; break;
        case "duplicates": p.has_duplicates = value; break;
        case "mimeType": p.mime_type = value; break;
        case "sort": p.ordering = value; break;
        default: p[key] = value;
      }
    }
    return p;
  }

  /** A document as returned by the API, compact and with names instead of ids. */
  async compact(doc, { content = false } = {}) {
    const name = async (kind, id) => (id == null ? null : (await this.findObject(kind, id))?.name ?? id);
    const tags = [];
    for (const id of doc.tags ?? []) tags.push(await name("tag", id));
    const fields = {};
    for (const { field, value } of doc.custom_fields ?? []) {
      const def = await this.findObject("customField", field);
      let v = value;
      if (def?.data_type === "select" && value != null) {
        v = (def.extra_data?.select_options ?? []).find((o) => o.id === value)?.label ?? value;
      }
      fields[def?.name ?? field] = v;
    }
    const out = {
      id: doc.id,
      title: doc.title,
      created: doc.created,
      added: doc.added?.slice(0, 10),
      modified: doc.modified,
      correspondent: await name("correspondent", doc.correspondent),
      document_type: await name("documentType", doc.document_type),
      storage_path: await name("storagePath", doc.storage_path),
      tags,
      asn: doc.archive_serial_number ?? undefined,
      custom_fields: Object.keys(fields).length ? fields : undefined,
      pages: doc.page_count ?? undefined,
      file: doc.original_file_name,
      mime_type: doc.mime_type,
    };
    if (doc.__search_hit__) {
      out.score = Math.round(doc.__search_hit__.score * 1000) / 1000;
      out.highlights = stripHighlights(doc.__search_hit__.highlights);
      out.note_highlights = stripHighlights(doc.__search_hit__.note_highlights);
    }
    if (content && doc.content != null) out.content = truncate(doc.content, content === true ? DEFAULT_CONTENT_CHARS : content);
    if (doc.notes?.length) {
      out.notes = doc.notes.map((n) => ({ id: n.id, note: n.note, created: n.created?.slice(0, 10), user: n.user?.username ?? n.user }));
    }
    return out;
  }

  /**
   * Searches documents. `filters`: see references/search.md (query, text, title, content,
   * tags, anyTags, notTags, inbox, correspondent, documentType, storagePath, createdFrom,
   * createdTo, addedFrom, customFields, moreLike, duplicates, sort, ids, or raw API params).
   * `limit` (default 25, Infinity for all), `content` (true or max characters per document).
   * Returns {count, documents}.
   */
  async find(filters = {}, { limit = 25, content = false, compact = true } = {}) {
    const params = await this.params(filters);
    if (!content) params.fields = LIST_FIELDS.join(",");
    const docs = await this.all("/api/documents/", params, { limit, pageSize: content ? 25 : 250 });
    const documents = [];
    for (const doc of docs) documents.push(compact ? await this.compact(doc, { content }) : doc);
    return { count: docs.count, documents };
  }

  /** Only the number of matching documents. */
  async count(filters = {}) {
    const params = await this.params(filters);
    return (await this.get("/api/documents/", { ...params, page_size: 1, fields: "id" })).count;
  }

  /** Only the ids of all matching documents. */
  async ids(filters = {}) {
    const params = await this.params(filters);
    return (await this.all("/api/documents/", { ...params, fields: "id" }, { pageSize: 1000 })).map((d) => d.id);
  }

  /**
   * One document with its text content (`content`: true = up to 20000 characters, a number,
   * or false) and notes.
   */
  async doc(id, { content = true } = {}) {
    const doc = await this.get(`/api/documents/${id}/`);
    return this.compact(doc, { content });
  }

  /** Custom field values as the API expects them; `values` maps field names (or ids) to values, null removes. */
  async customFieldValues(values) {
    const out = [];
    for (const [ref, value] of Object.entries(values)) {
      const def = await this.findObject("customField", /^\d+$/.test(ref) ? Number(ref) : ref);
      if (!def) {
        const similar = similarNames(ref, await this.objects("customField"));
        const names = similar.length ? similar : (await this.objects("customField")).map((f) => f.name);
        throw new Error(`no custom field named ${JSON.stringify(ref)}; existing: ${names.join(", ") || "none"} (create it with createObject("customField", { name, data_type }))`);
      }
      let v = value;
      if (v != null && def.data_type === "select" && typeof v === "string") {
        const option = (def.extra_data?.select_options ?? []).find((o) => o.label.toLowerCase() === v.toLowerCase() || o.id === v);
        if (!option) throw new Error(`"${v}" is not an option of ${def.name}: ${(def.extra_data?.select_options ?? []).map((o) => o.label).join(", ")}`);
        v = option.id;
      } else if (v != null && def.data_type === "monetary" && typeof v === "number") {
        v = `${def.extra_data?.default_currency ?? ""}${v.toFixed(2)}`;
      } else if (v != null && def.data_type === "documentlink") {
        v = list(v);
      }
      out.push({ field: def.id, value: v, remove: value === null });
    }
    return out;
  }

  /** Throws if the document's `modified` differs from `modified` (somebody changed it meanwhile). */
  async assertUnchanged(id, modified) {
    const now = (await this.get(`/api/documents/${id}/`, { fields: "modified" })).modified;
    if (new Date(now).getTime() !== new Date(modified).getTime()) {
      throw new Error(`document ${id} was changed since you read it (modified ${now}, expected ${modified}); read it again and redo the proposal`);
    }
  }

  /**
   * Changes one document. `changes`: title, created ("YYYY-MM-DD"), asn, correspondent,
   * documentType, storagePath (name, id or null), tags (replaces all), addTags, removeTags,
   * customFields ({name: value}; null removes the field), removeInboxTags (true), owner.
   * Names that do not exist yet are created for tags/correspondents/document types when
   * `create: true`. With `unchangedSince` (the `modified` of the document as you read it) it
   * throws, before changing anything, if the document was changed since. Returns the updated
   * compact document.
   */
  async update(id, changes, { create = false, unchangedSince } = {}) {
    if (unchangedSince) await this.assertUnchanged(id, unchangedSince);
    const body = {};
    const opts = { create };
    let current;
    const load = async () => (current ??= await this.get(`/api/documents/${id}/`, { fields: "id,tags,custom_fields" }));
    for (const [key, value] of Object.entries(changes)) {
      if (value === undefined) continue;
      switch (key) {
        case "title": body.title = value; break;
        case "created": body.created = value; break;
        case "asn": body.archive_serial_number = value; break;
        case "owner": body.owner = value; break;
        case "correspondent": body.correspondent = await this.resolve("correspondent", value, opts); break;
        case "documentType": body.document_type = await this.resolve("documentType", value, opts); break;
        case "storagePath": body.storage_path = await this.resolve("storagePath", value); break;
        case "tags": body.tags = await this.resolveAll("tag", value, opts); break;
        case "addTags":
        case "removeTags": break; // below, after `tags`
        case "customFields": {
          const fields = new Map(((await load()).custom_fields ?? []).map((f) => [f.field, f.value]));
          for (const f of await this.customFieldValues(value)) {
            if (f.remove) fields.delete(f.field);
            else fields.set(f.field, f.value);
          }
          body.custom_fields = [...fields].map(([field, value]) => ({ field, value }));
          break;
        }
        case "removeInboxTags": body.remove_inbox_tags = value; break;
        default: body[key] = value; // raw API field
      }
    }
    if (changes.addTags || changes.removeTags) {
      const tags = new Set(body.tags ?? (await load()).tags);
      for (const t of await this.resolveAll("tag", changes.addTags, opts)) tags.add(t);
      for (const t of await this.resolveAll("tag", changes.removeTags)) tags.delete(t);
      body.tags = [...tags];
    }
    const updated = await this.patch(`/api/documents/${id}/`, body);
    return this.compact(updated);
  }

  /** Raw bulk edit (runs as a background task): returns {result, task_id}. */
  bulkEdit(ids, method, parameters = {}) {
    return this.post("/api/documents/bulk_edit/", { documents: list(ids), method, parameters });
  }

  async addTags(ids, tags, { create = false } = {}) {
    return this.bulkEdit(ids, "modify_tags", { add_tags: await this.resolveAll("tag", tags, { create }), remove_tags: [] });
  }
  async removeTags(ids, tags) {
    return this.bulkEdit(ids, "modify_tags", { add_tags: [], remove_tags: await this.resolveAll("tag", tags) });
  }
  async modifyTags(ids, { add = [], remove = [] }, { create = false } = {}) {
    return this.bulkEdit(ids, "modify_tags", {
      add_tags: await this.resolveAll("tag", add, { create }),
      remove_tags: await this.resolveAll("tag", remove),
    });
  }
  async setCorrespondent(ids, ref, { create = false } = {}) {
    return this.bulkEdit(ids, "set_correspondent", { correspondent: await this.resolve("correspondent", ref, { create }) });
  }
  async setDocumentType(ids, ref, { create = false } = {}) {
    return this.bulkEdit(ids, "set_document_type", { document_type: await this.resolve("documentType", ref, { create }) });
  }
  async setStoragePath(ids, ref) {
    return this.bulkEdit(ids, "set_storage_path", { storage_path: await this.resolve("storagePath", ref) });
  }
  /** Sets custom fields ({name: value}) on many documents; null values are removed. */
  async setCustomFields(ids, values) {
    const add = {};
    const remove = [];
    for (const f of await this.customFieldValues(values)) {
      if (f.remove) remove.push(f.field);
      else add[f.field] = f.value;
    }
    return this.bulkEdit(ids, "modify_custom_fields", { add_custom_fields: add, remove_custom_fields: remove });
  }
  /** Moves documents to the trash (restorable in the web UI under Trash). */
  deleteDocuments(ids) {
    return this.post("/api/documents/delete/", { documents: list(ids) });
  }
  /** Re-runs OCR and parsing on documents. */
  reprocess(ids) {
    return this.post("/api/documents/reprocess/", { documents: list(ids) });
  }

  // ------------------------------------------------------------ extras

  async notes(id) {
    return (await this.get(`/api/documents/${id}/notes/`)).map((n) => ({ id: n.id, note: n.note, created: n.created?.slice(0, 10), user: n.user?.username ?? n.user }));
  }
  addNote(id, note) {
    return this.post(`/api/documents/${id}/notes/`, { note });
  }
  deleteNote(id, noteId) {
    return this.delete(`/api/documents/${id}/notes/`, { id: noteId });
  }

  /** Paperless' own (non-LLM) classifier suggestions for a document, as names. */
  async suggestions(id) {
    const s = await this.get(`/api/documents/${id}/suggestions/`);
    const names = async (kind, ids) => {
      const out = [];
      for (const i of ids ?? []) out.push((await this.findObject(kind, i))?.name ?? i);
      return out;
    };
    return {
      correspondents: await names("correspondent", s.correspondents),
      documentTypes: await names("documentType", s.document_types),
      tags: await names("tag", s.tags),
      storagePaths: await names("storagePath", s.storage_paths),
      dates: s.dates,
    };
  }

  /** Documents similar to document `id`. */
  similar(id, { limit = 10 } = {}) {
    return this.find({ moreLike: id }, { limit });
  }

  /** The change history (audit log) of a document. */
  history(id) {
    return this.get(`/api/documents/${id}/history/`);
  }

  /**
   * Uploads a file from the work directory. `meta`: title, created, correspondent,
   * documentType, storagePath, tags (names or ids), asn, customFields ({name: value}).
   * Returns the consumption task id; check it with task(id).
   */
  async upload(path, meta = {}) {
    if (!fs.exists(path)) throw new Error(`no file ${path} in the work directory (copy it there first)`);
    const form = { document: { file: path } };
    if (meta.title) form.title = meta.title;
    if (meta.created) form.created = meta.created;
    if (meta.correspondent != null) form.correspondent = await this.resolve("correspondent", meta.correspondent, { create: meta.create });
    if (meta.documentType != null) form.document_type = await this.resolve("documentType", meta.documentType, { create: meta.create });
    if (meta.storagePath != null) form.storage_path = await this.resolve("storagePath", meta.storagePath);
    if (meta.tags) form.tags = await this.resolveAll("tag", meta.tags, { create: meta.create });
    if (meta.asn != null) form.archive_serial_number = meta.asn;
    if (meta.customFields) {
      const values = {};
      for (const f of await this.customFieldValues(meta.customFields)) if (!f.remove) values[f.field] = f.value;
      form.custom_fields = JSON.stringify(values);
    }
    return this.request("POST", "/api/documents/post_document/", { form });
  }

  /** A background task (upload, bulk edit): {status, type, result, document, ...}. */
  async task(taskId) {
    const data = await this.get("/api/tasks/", { task_id: taskId });
    const task = Array.isArray(data) ? data[0] : data.results?.[0];
    if (!task) return { task_id: taskId, status: "unknown (not found yet; try again shortly)" };
    return compactTask(task);
  }

  /** Recent tasks, newest first; `filters` e.g. {status: "failure"} or {task_type: "consume_file"}. */
  async tasks(filters = {}, { limit = 20 } = {}) {
    return (await this.all("/api/tasks/", { ordering: "-date_created", ...filters }, { limit })).map(compactTask);
  }

  /**
   * Saves a document's file to the work directory (default `paperless/<id>.<ext>`): the archived
   * PDF, or the original with `original: true` (`paperless/<id>-original.<ext>`). Returns
   * {path, bytes, type}.
   */
  async download(id, path, { original = false } = {}) {
    if (!path) {
      const doc = await this.get(`/api/documents/${id}/`, { fields: "id,original_file_name,archived_file_name" });
      const name = original ? doc.original_file_name : doc.archived_file_name ?? doc.original_file_name;
      const ext = (name?.match(/\.[A-Za-z0-9]+$/)?.[0] ?? ".pdf").toLowerCase();
      path = `paperless/${id}${original ? "-original" : ""}${ext}`;
    }
    return this.request("GET", `/api/documents/${id}/download/`, { params: original ? { original: true } : {}, saveTo: path });
  }

  /** Saves the thumbnail (WebP) to the work directory, for the view_image tool. */
  thumbnail(id, path = `paperless/thumbs/${id}.webp`) {
    return this.request("GET", `/api/documents/${id}/thumb/`, { saveTo: path });
  }

  /** Creates a public share link; `expiration` is an ISO date or null (never). */
  async shareLink(id, { expiration = null, original = false } = {}) {
    const link = await this.post("/api/share_links/", {
      document: id,
      expiration,
      file_version: original ? "original" : "archive",
    });
    return { url: `${this.base}/share/${link.slug}`, expiration: link.expiration, id: link.id };
  }

  /** Document counts, inbox count, file types etc. */
  stats() {
    return this.get("/api/statistics/");
  }
}
