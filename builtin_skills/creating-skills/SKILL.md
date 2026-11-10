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
4. Move long material to `references/*.md` and link it relatively
   (`See [details](references/REFERENCE.md)`), one level deep. Put runnable code in `scripts/`
   and tell in the body how to run it, e.g. `AGENT --js skills/<skill-name>/scripts/run.js [args]`
   (`AGENT` is the agent executable from the system prompt; script arguments are in `globalThis.ARGS`).
5. Check it: run `AGENT --js skills/using-skills/scripts/use_skill.js <skill-name>`. It must
   print your instructions. The skill appears in the system prompt after the agent is restarted.
   A skill with invalid frontmatter is skipped with a warning at startup.

Do not edit or delete the built-in skills `using-skills`, `creating-skills` and `subagents`;
they are restored on every start.
