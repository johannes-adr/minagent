// System prompt component: how to load skills and the list of available skills.
// ARGS[0] is the path of the agent executable.
import fs from "node:fs";

const ROOT = "skills";
const exe = ARGS[0];

const escape = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// name and description from the frontmatter of skills/<name>/SKILL.md; invalid skills
// are skipped with a warning (stderr, shown at startup)
function readSkill(dir) {
  const location = `${ROOT}/${dir}/SKILL.md`;
  let text;
  try {
    text = fs.readFileSync(location, "utf8");
  } catch {
    return null; // not a skill directory
  }
  const skip = (why) => {
    console.error(`skills: ignoring ${location}: ${why}`);
    return null;
  };
  const front = text.match(/^---\r?\n([\s\S]*?)\r?\n---/);
  if (!front) return skip("missing frontmatter");
  const meta = {};
  for (const line of front[1].split(/\r?\n/)) {
    const kv = line.match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    if (kv) meta[kv[1]] = kv[2].replace(/^(["'])(.*)\1$/, "$2");
  }
  if (!meta.name || !/^[a-z0-9]+(-[a-z0-9]+)*$/.test(meta.name) || meta.name.length > 64)
    return skip("`name` must be 1-64 lowercase letters, digits and single hyphens");
  if (meta.name !== dir) return skip(`name \`${meta.name}\` does not match directory \`${dir}\``);
  if (!meta.description || meta.description.length > 1024)
    return skip("`description` must be 1-1024 characters");
  return { name: meta.name, description: meta.description, location };
}

export default function skills() {
  const found = fs.readdirSync(ROOT).sort().map(readSkill).filter(Boolean);
  if (found.length === 0) return "";
  const load = `${exe} --js ${ROOT}/using-skills/scripts/use_skill.js`;
  const list = found
    .map(
      (s) =>
        `<skill>\n<name>${escape(s.name)}</name>\n<description>${escape(s.description)}</description>\n<location>${escape(s.location)}</location>\n</skill>`,
    )
    .join("\n");
  return (
    "# Skills\n\n" +
    "Skills are specialised instructions stored as files. They are NOT tools: never call a skill by name as a tool. " +
    "When a task matches a skill's description, you MUST first load its instructions by running this with the `execute` tool, " +
    "and then follow what it prints (do not answer from the description alone):\n\n" +
    `\`${load} <skill-name>\`\n\n` +
    `Example: \`${load} creating-skills\`\n\n` +
    `Other JavaScript files are run with \`${exe} --js <file> [args]\`.\n\n` +
    `<available_skills>\n${list}\n</available_skills>`
  );
}
