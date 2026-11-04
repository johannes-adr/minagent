// Usage: agent --js skills/using-skills/scripts/use_skill.js [skillname]
// Prints the instructions of a skill (SKILL.md without frontmatter), or lists all skills.
import fs from "node:fs";

const ROOT = "skills";

function parse(text) {
  const m = text.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
  if (!m) return { meta: {}, body: text };
  const meta = {};
  for (const line of m[1].split(/\r?\n/)) {
    const kv = line.match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    if (kv) meta[kv[1]] = kv[2].replace(/^(["'])(.*)\1$/, "$2");
  }
  return { meta, body: m[2] };
}

function exists(path) {
  try {
    fs.statSync(path);
    return true;
  } catch {
    return false;
  }
}

function list() {
  if (!exists(ROOT)) return [];
  return fs
    .readdirSync(ROOT)
    .filter((n) => exists(`${ROOT}/${n}/SKILL.md`))
    .sort();
}

function describe(name) {
  const { meta } = parse(fs.readFileSync(`${ROOT}/${name}/SKILL.md`, "utf8"));
  return `- ${name}: ${meta.description ?? ""}`;
}

const name = globalThis.ARGS?.[0];

if (!name) {
  console.log("Available skills:\n" + list().map(describe).join("\n"));
} else if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(name) || !list().includes(name)) {
  console.error(`Unknown skill: ${name}\nAvailable skills:\n` + list().map(describe).join("\n"));
  throw new Error(`unknown skill: ${name}`);
} else {
  const { body } = parse(fs.readFileSync(`${ROOT}/${name}/SKILL.md`, "utf8"));
  console.log(`# Skill: ${name}\nSkill directory: ${ROOT}/${name}/\n\n${body.trim()}`);
}
