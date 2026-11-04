---
name: using-skills
description: Explains how to load and use skills (specialised instructions) via the bundled use_skill.js script. Use whenever a task matches a skill listed under available_skills, or when you want to see what a skill offers.
license: MIT
---

# Using skills

The system prompt lists every available skill with its name and a short description.
Only that summary is loaded up front. To get the full instructions of a skill, load it:

```sh
AGENT --js skills/using-skills/scripts/use_skill.js <skillname>
```

`AGENT` is the agent executable given in the system prompt. Run the command with the
`execute` tool. Example for a skill called `pdf-processing`:

```sh
AGENT --js skills/using-skills/scripts/use_skill.js pdf-processing
```

The script prints the skill's `SKILL.md` (without the frontmatter) and the directory
of the skill. Then:

1. Read and follow the instructions it printed.
2. Paths mentioned in the instructions (`scripts/...`, `references/...`, `assets/...`)
   are relative to the skill directory, i.e. `skills/<skillname>/`.
3. Load further files of the skill (references, assets) only when you need them.
4. Run a skill's scripts with `AGENT --js skills/<skillname>/scripts/<script>.js [args]`
   if they are JavaScript, otherwise with the interpreter that fits.

Without an argument the script lists all available skills.
An unknown name prints an error and the list of valid names.
