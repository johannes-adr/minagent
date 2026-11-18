---
name: creating-skills
description: Explains how to create a new skill (a SKILL.md with optional scripts, references and assets) following the Agent Skills format. Use when the user asks to create, write or add a skill, or when a repeatable workflow should be saved for later.
license: MIT
---

# Creating a skill

A skill is a directory in `skills/` containing a `SKILL.md`:

```
skills/<skill-name>/
├── SKILL.md        # required: frontmatter + instructions
├── scripts/        # optional: executable code
├── references/     # optional: detailed docs, loaded on demand
└── assets/         # optional: templates, data, images
```

## Steps

1. Pick a name: 1-64 characters, only lowercase letters, digits and hyphens, no leading,
   trailing or double hyphens (`pdf-processing`, not `PDF_Processing`).
   The directory name must equal the name.
2. Create `skills/<skill-name>/SKILL.md` with this frontmatter, then the instructions:

   ```markdown
   ---
   name: pdf-processing
   description: Extracts text and tables from PDFs and merges them. Use when the user mentions PDFs or document extraction.
   ---

   # PDF processing
   1. Step one ...
   ```

   - `name` (required) and `description` (required, 1-1024 characters) go on single lines.
   - The description is the only thing shown in the system prompt, so say both what the
     skill does and when to use it, with keywords a task would contain.
   - Optional fields: `license`, `compatibility` (max 500 characters, environment
     requirements), `metadata` (string key/value map).
3. Write the body as instructions for yourself: concrete steps, example inputs and outputs,
   edge cases. Keep it under 500 lines (about 5000 tokens).
4. Move long material to `references/*.md`, one file per subtopic, one level deep. List each
   file in the body with one line saying when it is needed; the agent then loads it only when a
   task needs it, with the `use_skill` tool and `file` set to its path (e.g. `references/api.md`).
   Put runnable code in `scripts/` as shell scripts and tell in the body how to run them, e.g.
   `sh skills/<skill-name>/scripts/run.sh [args]` (script arguments are `$1`, `$2`, ...).
5. Optionally ship JavaScript modules in `scripts/`, see below.
6. Check it: call the `use_skill` tool with `name` set to `<skill-name>`. It must return your
   instructions. The skill appears in the system prompt after the agent is restarted.
   A skill with invalid frontmatter is skipped with a warning at startup.

Do not edit or delete the built-in skill `creating-skills`; it is restored on every start.

## JavaScript modules in `scripts/`

Besides shell scripts, `scripts/` may contain JavaScript ES modules that the
`execute_javascript` tool imports. They are not Node scripts: only `console`, `fetch`, `fs`
and relative imports inside the skill exist (no `require`, npm packages or Node APIs).
Skills are read-only for scripts. Export functions:

```js
// skills/github/scripts/github.js
export async function listIssues(repo) {
  const r = await fetch(`https://api.github.com/repos/${repo}/issues`, { secret: "GITHUB" });
  return r.json();
}
```

Document the exported functions in `SKILL.md` (signature, result, the `hosts` the call
needs), so that the agent can use them:

```markdown
Use in `execute_javascript` (with `hosts: ["api.github.com"]`):
    import { listIssues } from "skill:github/scripts/github.js";
    const issues = await listIssues("owner/repo"); // [{ number, title, ... }]
```

Other files of the skill can be read with `fs.readText("skill:github/assets/template.md")`.
Modules import each other relatively (`./util.js`). Because these modules are not Node
scripts, say so in the `compatibility` frontmatter field, e.g.
`compatibility: JavaScript modules for the execute_javascript tool (not Node)`.
Shell scripts are documented and run as before, with `execute`.
