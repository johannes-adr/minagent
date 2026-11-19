// Markdown (CommonMark/GFM subset). All text is escaped first and links are limited to
// http(s)/mailto. Images are shown as links: loading a URL the model wrote would let it phone home.

const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const SAFE_URL = /^(https?:\/\/|mailto:)/i;
const link = (url, html) => `<a href="${url}" target="_blank" rel="noopener noreferrer">${html}</a>`;
function inline(src) {
  const slots = [];
  const hold = (html) => `\u0001${slots.push(html) - 1}\u0001`;
  let s = src.replace(/\u0001/g, "");
  // Code spans and escapes first: nothing inside them is formatted.
  s = s.replace(/(`+)(?!`)([\s\S]*?[^`])\1(?!`)/g, (_, __, code) => hold(`<code>${esc(code.replace(/^ (.*[^ ].*) $/s, "$1"))}</code>`));
  s = s.replace(/\\([\\`*_{}\[\]()#+\-.!~|<>])/g, (_, c) => hold(esc(c)));
  // Links. Images are shown as links: loading a URL the model wrote would let it phone home.
  s = s.replace(/<((?:https?:\/\/|mailto:)[^\s<>]+)>/gi, (_, url) => hold(link(esc(url), esc(url))));
  const target = String.raw`\(\s*<?((?:[^\s()<>]|\([^\s()]*\))*)>?(?:\s+"[^"]*")?\s*\)`;
  s = s.replace(new RegExp(String.raw`!\[([^\]]*)\]` + target, "g"), (_, alt, url) =>
    SAFE_URL.test(url) ? hold(link(esc(url), esc(alt || url))) : hold(esc(alt)));
  s = s.replace(new RegExp(String.raw`\[((?:[^\[\]]|\[[^\]]*\])+)\]` + target, "g"), (_, text, url) =>
    SAFE_URL.test(url) ? hold(link(esc(url), inline(text))) : hold(inline(text)));
  s = esc(s);
  s = s.replace(/(^|[\s(])(https?:\/\/[^\s<]*[^\s<.,;:!?)\]'"])/g, (_, before, url) => before + hold(link(url, url)));
  s = s.replace(/\*\*\*(?=\S)([\s\S]*?\S)\*\*\*/g, "<strong><em>$1</em></strong>")
       .replace(/\*\*(?=\S)([\s\S]*?\S)\*\*/g, "<strong>$1</strong>")
       .replace(/(^|[^\w])__(?=\S)([\s\S]*?\S)__(?!\w)/g, "$1<strong>$2</strong>")
       .replace(/(^|[^\w*])\*(?=[^\s*])([^*]*?[^\s*])\*(?![\w*])/g, "$1<em>$2</em>")
       .replace(/(^|[^\w])_(?=[^\s_])([^_]*?[^\s_])_(?![\w])/g, "$1<em>$2</em>")
       .replace(/~~(?=\S)([\s\S]*?\S)~~/g, "<del>$1</del>")
       .replace(/( {2,}|\\)\n/g, "<br>");
  while (/\u0001\d+\u0001/.test(s)) s = s.replace(/\u0001(\d+)\u0001/g, (_, i) => slots[i]);
  return s;
}

const FENCE = /^(\s*)(`{3,}|~{3,})\s*([^\s`]*)[^`]*$/;
const HEADING = /^ {0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/;
const RULE = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/;
const QUOTE = /^ {0,3}>/;
const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])(?:\s+(.*)|$)/;
const TABLE_SEP = /^\s*\|?\s*:?-+:?\s*(?:\|\s*:?-+:?\s*)*\|?\s*$/;
const blank = (line) => !line.trim();
const startsBlock = (line) => FENCE.test(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line) || ITEM.test(line);

function codeBlock(code, lang) {
  return `<div class="code"><button class="copy">Copy</button>${lang ? `<span class="lang">${esc(lang)}</span>` : ""}<pre><code>${esc(code)}</code></pre></div>`;
}
const cells = (line) => line.trim().replace(/^\||\|$/g, "").split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, "|"));

function table(lines, i) {
  const head = cells(lines[i]);
  const align = cells(lines[i + 1]).map((c) => (/^:-+:$/.test(c) ? "center" : /-:$/.test(c) ? "right" : /^:-/.test(c) ? "left" : ""));
  const cell = (tag, text, n) => `<${tag}${align[n] ? ` style="text-align:${align[n]}"` : ""}>${inline(text)}</${tag}>`;
  let html = `<div class="table-wrap"><table><thead><tr>${head.map((c, n) => cell("th", c, n)).join("")}</tr></thead>`;
  const rows = [];
  for (i += 2; i < lines.length && !blank(lines[i]) && lines[i].includes("|") && !startsBlock(lines[i]); i++) {
    const row = cells(lines[i]);
    rows.push(`<tr>${head.map((_, n) => cell("td", row[n] ?? "", n)).join("")}</tr>`);
  }
  return [html + (rows.length ? `<tbody>${rows.join("")}</tbody>` : "") + "</table></div>", i];
}

function list(lines, i) {
  const first = lines[i].match(ITEM);
  const base = first[1].length, ordered = /\d/.test(first[2]);
  const start = ordered ? parseInt(first[2], 10) : 1;
  const items = [];
  let loose = false;
  while (i < lines.length) {
    const m = lines[i].match(ITEM);
    // A sibling is a marker of the same kind that is not indented into the current item.
    if (!m || m[1].length > base + 1 || /\d/.test(m[2]) !== ordered) break;
    const content = m[1].length + m[2].length + 1;
    const body = [m[3] ?? ""];
    i++;
    for (; i < lines.length; i++) {
      const line = lines[i];
      if (blank(line)) {
        let j = i;
        while (j < lines.length && blank(lines[j])) j++;
        if (j < lines.length && lines[j].match(/^\s*/)[0].length > base + 1) { body.push(""); continue; }
        // A blank line before the next item of this list makes the list loose.
        const next = j < lines.length && lines[j].match(ITEM);
        if (next && next[1].length <= base + 1 && /\d/.test(next[2]) === ordered) loose = true;
        i = j; break;
      }
      const indent = line.match(/^\s*/)[0].length;
      if (indent > base + 1) body.push(line.slice(Math.min(indent, content)));
      else if (!startsBlock(line) && !blank(body[body.length - 1])) body.push(line.trim()); // lazy continuation
      else break;
    }
    while (body.length && blank(body[body.length - 1])) body.pop();
    items.push(body);
    if (i < lines.length && !ITEM.test(lines[i])) break;
  }
  const html = items.map((body) => {
    let task = "";
    const t = body[0].match(/^\[([ xX])\]\s+(.*)/);
    if (t) { task = `<input type="checkbox" disabled${t[1] === " " ? "" : " checked"}> `; body[0] = t[2]; }
    let inner = blocks(body);
    if (!loose) inner = inner.replace(/^<p>([\s\S]*?)<\/p>/, "$1");
    else if (task) inner = inner.replace(/^<p>/, "<p>" + task), task = "";
    return `<li${t ? ' class="task"' : ""}>${task}${inner}</li>`;
  }).join("");
  const tag = ordered ? "ol" : "ul";
  return [`<${tag}${ordered && start !== 1 ? ` start="${start}"` : ""}>${html}</${tag}>`, i];
}

function blocks(lines) {
  const out = [];
  let i = 0, m;
  while (i < lines.length) {
    const line = lines[i];
    if (blank(line)) { i++; continue; }
    if ((m = line.match(FENCE))) {
      const [, indent, fence, lang] = m;
      const close = new RegExp(`^\\s*${fence[0]}{${fence.length},}\\s*$`);
      const body = [];
      for (i++; i < lines.length && !close.test(lines[i]); i++) body.push(lines[i].startsWith(indent) ? lines[i].slice(indent.length) : lines[i].trimStart());
      i++; // the closing fence; a block still streaming has none and runs to the end
      out.push(codeBlock(body.join("\n"), lang));
    } else if ((m = line.match(HEADING))) {
      out.push(`<h${m[1].length}>${inline(m[2])}</h${m[1].length}>`); i++;
    } else if (RULE.test(line)) {
      out.push("<hr>"); i++;
    } else if (QUOTE.test(line)) {
      const body = [];
      for (; i < lines.length && !blank(lines[i]) && (QUOTE.test(lines[i]) || body.length); i++) body.push(lines[i].replace(/^ {0,3}>\s?/, ""));
      out.push(`<blockquote>${blocks(body)}</blockquote>`);
    } else if (line.includes("|") && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes("-") && cells(line).length === cells(lines[i + 1]).length) {
      const [html, next] = table(lines, i); out.push(html); i = next;
    } else if (ITEM.test(line)) {
      const [html, next] = list(lines, i); out.push(html); i = next;
    } else {
      const para = [line];
      for (i++; i < lines.length && !blank(lines[i]) && !startsBlock(lines[i]); i++) para.push(lines[i]);
      out.push(`<p>${inline(para.join("\n").trim())}</p>`);
    }
  }
  return out.join("");
}
export function markdown(source) { return blocks(source.replace(/\r\n?/g, "\n").split("\n")); }
