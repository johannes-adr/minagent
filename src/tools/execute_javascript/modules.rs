//! ES modules of skills: `import { f } from "skill:<name>/<path>"`, read-only.
//!
//! Skills live in [`skills_root`], not in the working directory, because the script can
//! write to the working directory without approval but must never be able to change a skill.
//! Names are normalized to `skill:<name>/<path>` (no `.`/`..`), so a module is loaded and
//! evaluated once however it was imported. Files are read through [`fs::resolve`], the
//! same confinement as `fs` (no escape from the skills root, no `.env*`).

use super::fs;
use crate::tools::skills::{self, SkillFilter};
use std::cell::RefCell;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Ctx, Error, Module, Result};
use std::path::PathBuf;

/// The prefix of skill module specifiers and `fs` paths.
pub(super) const PREFIX: &str = "skill:";
const ONLY_SKILLS: &str = "only `skill:<name>/<path>` imports are supported";

thread_local! {
    /// The skills the script running on this thread may use. A script runs on one blocking
    /// thread from start to end, and `fs` and the module loader have no other way to know.
    static ALLOWED: RefCell<SkillFilter> = const { RefCell::new(SkillFilter::All) };
}

/// Runs `f` (a script) with only the skills `filter` allows.
pub(super) fn with_filter<R>(filter: SkillFilter, f: impl FnOnce() -> R) -> R {
    struct Restore(SkillFilter);
    impl Drop for Restore {
        fn drop(&mut self) {
            ALLOWED.replace(std::mem::take(&mut self.0));
        }
    }
    let _restore = Restore(ALLOWED.replace(filter));
    f()
}

fn check_allowed(name: &str) -> std::result::Result<(), String> {
    if ALLOWED.with(|allowed| allowed.borrow().allows(name)) {
        Ok(())
    } else {
        Err(format!("the skill {name:?} is not enabled"))
    }
}

/// Where the skills are stored; the only place that knows.
pub(super) fn skills_root() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_default()
        .join(skills::DIR)
}

/// Appends `path` to `stack`, resolving `.` and `..`. Fails when `..` would
/// leave the root.
fn normalize(path: &str, mut stack: Vec<String>) -> Option<Vec<String>> {
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            part => stack.push(part.to_owned()),
        }
    }
    Some(stack)
}

/// `skill:<name>/<path>` as `(name, normalized path segments)`. Fails if the name is not a
/// single segment or the path is empty or leaves the skill (`..` never pops the name).
fn parse_absolute(spec: &str) -> std::result::Result<(String, Vec<String>), String> {
    let rest = spec.strip_prefix(PREFIX).ok_or(ONLY_SKILLS)?;
    let (name, path) = rest
        .split_once('/')
        .ok_or_else(|| format!("{spec:?}: expected skill:<name>/<path>"))?;
    if name.is_empty() || name == "." || name == ".." || name.contains(['\\', ':']) {
        return Err(format!("{spec:?}: invalid skill name"));
    }
    check_allowed(name)?;
    let path = normalize(path, Vec::new())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| format!("{spec:?}: path must stay inside the skill"))?;
    Ok((name.to_owned(), path))
}

/// The canonical name of `spec` imported from module `base`.
pub(super) fn resolve_name(base: &str, spec: &str) -> std::result::Result<String, String> {
    let (skill, path) = if spec.starts_with(PREFIX) {
        parse_absolute(spec)?
    } else if base.starts_with(PREFIX) && (spec.starts_with("./") || spec.starts_with("../")) {
        let (skill, mut dir) = parse_absolute(base)?;
        dir.pop(); // the importing file
        let path = normalize(spec, dir)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| format!("{spec:?}: relative imports must stay inside the skill"))?;
        (skill, path)
    } else {
        return Err(format!("{spec:?}: {ONLY_SKILLS}"));
    };
    Ok(format!("{PREFIX}{skill}/{}", path.join("/")))
}

/// The path below the skills root of a canonical or `skill:` path (`demo/lib.js`).
pub(super) fn below_root(spec: &str) -> std::result::Result<&str, String> {
    match spec.strip_prefix(PREFIX) {
        Some(rest) if !rest.is_empty() => {
            check_allowed(rest.split('/').next().unwrap_or_default())?;
            Ok(rest)
        }
        _ => Err(format!("{spec:?}: expected skill:<name>/<path>")),
    }
}

pub(super) struct SkillResolver;

impl Resolver for SkillResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> Result<String> {
        resolve_name(base, name).map_err(|m| Error::new_resolving_message(base, name, m))
    }
}

/// Reads modules of the skills below `root`.
pub(super) struct SkillLoader {
    pub root: PathBuf,
}

impl Loader for SkillLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> Result<Module<'js, Declared>> {
        let fail = |m: String| Error::new_loading_message(name, m);
        if !(name.ends_with(".js") || name.ends_with(".mjs")) {
            return Err(fail("skill modules must be .js or .mjs files".to_owned()));
        }
        // Names reach the loader normalized, but confinement does not rely on that.
        let relative = below_root(name).map_err(fail)?;
        let file = fs::resolve(&self.root, relative).map_err(fail)?;
        let source = std::fs::read(file).map_err(|e| fail(e.to_string()))?;
        Module::declare(ctx.clone(), name, source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rquickjs::{CatchResultExt, Context, Runtime};
    use std::path::Path;

    #[test]
    fn skills_that_are_not_allowed_cannot_be_imported_or_read() {
        let off = SkillFilter::disabled(["demo".to_owned()]);
        with_filter(off, || {
            assert!(resolve_name("script", "skill:demo/lib.js").unwrap_err().contains("not enabled"));
            assert!(resolve_name("skill:demo/a.js", "./b.js").is_err());
            assert!(below_root("skill:demo/lib.js").unwrap_err().contains("not enabled"));
            assert!(resolve_name("script", "skill:other/lib.js").is_ok());
            assert_eq!(below_root("skill:other/x"), Ok("other/x"));
        });
        // The filter does not outlive the script.
        assert!(below_root("skill:demo/lib.js").is_ok());
    }

    #[test]
    fn specifiers_are_normalized() {
        let main = "script";
        let ok = |base, spec| resolve_name(base, spec).unwrap();
        assert_eq!(ok(main, "skill:demo/lib.js"), "skill:demo/lib.js");
        assert_eq!(
            ok(main, "skill:demo/./a//b/../lib.js"),
            "skill:demo/a/lib.js"
        );
        assert_eq!(ok("skill:demo/a/b.js", "./c.js"), "skill:demo/a/c.js");
        assert_eq!(ok("skill:demo/a/b.js", "../lib.js"), "skill:demo/lib.js");
        assert_eq!(
            ok("skill:demo/a/b.js", "skill:other/x.js"),
            "skill:other/x.js"
        );
    }

    #[test]
    fn bad_specifiers_are_rejected() {
        let main = "script";
        for (base, spec) in [
            (main, "skill:../x"),
            (main, "skill:demo/../../work/x.js"),
            (main, "skill:demo/../x.js"),
            (main, "skill:demo/../other/x.js"),
            ("skill:demo/a.js", "../../other/x.js"),
            (main, "skill:demo"),
            (main, "skill:demo/"),
            (main, "skill:/x.js"),
            (main, "skill:a:b/x.js"),
            (main, "lodash"),
            (main, "./lib.js"),
            (main, "../lib.js"),
            (main, "https://example.com/x.js"),
            (main, "/etc/passwd"),
            (main, "node:fs"),
            ("skill:demo/a.js", "../../x.js"),
            ("skill:demo/a/b.js", "../../../x.js"),
            ("skill:demo/a.js", "lodash"),
            ("skill:demo/a.js", "/abs.js"),
            ("skill:demo/a.js", "https://example.com/x.js"),
        ] {
            assert!(resolve_name(base, spec).is_err(), "{base} -> {spec}");
        }
        let message = resolve_name("script", "lodash").unwrap_err();
        assert!(
            message.contains("only `skill:<name>/<path>` imports"),
            "{message}"
        );
    }

    /// A skills root with `demo` (modules, an asset), `other` and traps.
    fn skills_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agent_js_skills_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let write = |path: &str, text: &str| {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        write(
            "demo/lib.js",
            "globalThis.loads = (globalThis.loads ?? 0) + 1; export const add = (a, b) => a + b;",
        );
        write(
            "demo/sub/x.js",
            "import { add } from '../lib.js'; export const inc = (n) => add(n, 1);",
        );
        write(
            "demo/sub/escape.js",
            "import '../../other/y.js'; export const v = 1;",
        );
        write(
            "demo/sub/abs.js",
            "import { y } from 'skill:other/y.js'; export const v = y;",
        );
        write("demo/sub/bare.js", "import 'lodash'; export const v = 1;");
        write("demo/.env", "SECRET=1");
        write("other/y.js", "export const y = 'other';");
        write("outside/z.js", "export const z = 1;");
        std::fs::write(dir.join("demo/data.txt"), "not js").unwrap();
        #[cfg(unix)]
        {
            let outside = std::env::temp_dir()
                .join(format!("agent_js_skills_out_{tag}_{}", std::process::id()));
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("evil.js"), "export const evil = 1;").unwrap();
            std::fs::write(outside.join("env.js"), "export const e = 1;").unwrap();
            std::os::unix::fs::symlink(outside.join("evil.js"), dir.join("demo/link.js")).unwrap();
            std::os::unix::fs::symlink(dir.join("demo/.env"), dir.join("demo/env.js")).unwrap();
        }
        dir
    }

    /// Runs `code` as the main script against the skills in `root`; the error message or
    /// `globalThis.result` stringified.
    fn run(root: &Path, code: &str) -> std::result::Result<String, String> {
        let runtime = Runtime::new().unwrap();
        runtime.set_loader(
            SkillResolver,
            SkillLoader {
                root: root.to_path_buf(),
            },
        );
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            let promise = Module::evaluate(ctx.clone(), "script", code)
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            promise
                .finish::<()>()
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            ctx.eval::<String, _>("String(globalThis.result) + '/' + String(globalThis.loads)")
                .catch(&ctx)
                .map_err(|e| e.to_string())
        })
    }

    #[test]
    fn named_exports_are_imported_and_relative_imports_work() {
        let root = skills_dir("import");
        let out = run(
            &root,
            "import { add } from 'skill:demo/lib.js'; globalThis.result = add(2, 3);",
        );
        assert_eq!(out.unwrap(), "5/1");
        let out = run(
            &root,
            "import { inc } from 'skill:demo/sub/x.js'; globalThis.result = inc(1);",
        );
        assert_eq!(out.unwrap(), "2/1");
        let out = run(
            &root,
            "import { v } from 'skill:demo/sub/abs.js'; globalThis.result = v;",
        );
        assert_eq!(out.unwrap(), "other/undefined");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_module_imported_twice_is_evaluated_once() {
        let root = skills_dir("once");
        let out = run(
            &root,
            "import { add } from 'skill:demo/lib.js';
             import { add as add2 } from 'skill:demo/sub/../lib.js';
             import { inc } from 'skill:demo/sub/x.js';
             globalThis.result = add(1, 1) + add2(1, 1) + inc(0);",
        );
        assert_eq!(out.unwrap(), "5/1");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn escapes_and_unsupported_imports_fail() {
        let root = skills_dir("fail");
        for (code, expected) in [
            ("import 'skill:../x';", "invalid skill name"),
            ("import 'skill:demo/../../work/x.js';", "inside the skill"),
            ("import 'skill:demo/sub/escape.js';", "inside the skill"),
            (
                "import 'skill:demo/sub/bare.js';",
                "only `skill:<name>/<path>` imports",
            ),
            ("import 'lodash';", "only `skill:<name>/<path>` imports"),
            ("import './lib.js';", "only `skill:<name>/<path>` imports"),
            (
                "import 'https://example.com/a.js';",
                "only `skill:<name>/<path>` imports",
            ),
            ("import 'skill:demo/.env';", ".env"),
            ("import 'skill:demo/missing.js';", "No such file"),
            ("import 'skill:demo/data.txt';", ".js or .mjs"),
        ] {
            let message = run(&root, code).unwrap_err();
            assert!(message.contains(expected), "{code}: {message}");
        }
        #[cfg(unix)]
        for name in ["link.js", "env.js"] {
            let code = format!("import 'skill:demo/{name}';");
            assert!(run(&root, &code).is_err(), "{name}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
