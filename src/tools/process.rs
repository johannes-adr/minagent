//! Helpers for the tools that start processes (`execute`).

use std::process::ExitStatus;

pub(crate) fn exit_line(status: &ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code: {code}"),
        None => format!("exit code: none ({status})"),
    }
}

/// Characters kept per output stream: the first `OUTPUT_HEAD` and the last
/// `OUTPUT_LIMIT - OUTPUT_HEAD` (errors and summaries are usually at the end).
const OUTPUT_LIMIT: usize = 30_000;
const OUTPUT_HEAD: usize = 10_000;

/// Keeps the first `OUTPUT_HEAD` and the last characters of `text` up to `OUTPUT_LIMIT`.
fn truncate_middle(text: &str) -> String {
    let total = text.chars().count();
    if total <= OUTPUT_LIMIT {
        return text.to_string();
    }
    let byte_at = |n: usize| text.char_indices().nth(n).map_or(text.len(), |(i, _)| i);
    let head_end = byte_at(OUTPUT_HEAD);
    let tail_start = byte_at(total - (OUTPUT_LIMIT - OUTPUT_HEAD));
    format!(
        "{}\n[... {} characters omitted; filter the output (grep, head, tail) to see them ...]\n{}",
        &text[..head_end],
        total - OUTPUT_LIMIT,
        &text[tail_start..]
    )
}

pub(crate) fn format_output(exit: &str, stdout: &[u8], stderr: &[u8]) -> String {
    let mut out = exit.to_string();
    for (name, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        let text = String::from_utf8_lossy(bytes);
        if !text.trim().is_empty() {
            out.push_str(&format!("\n{name}:\n{}", truncate_middle(text.trim_end())));
        }
    }
    if stdout.is_empty() && stderr.is_empty() {
        out.push_str("\n(no output)");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_output_is_cut_in_the_middle() {
        let text = format!(
            "{}{}{}",
            "a".repeat(OUTPUT_HEAD),
            "b".repeat(50_000),
            "c".repeat(100)
        );
        let cut = truncate_middle(&text);
        assert!(cut.starts_with(&"a".repeat(OUTPUT_HEAD)), "head kept");
        assert!(cut.ends_with(&"c".repeat(100)), "tail kept");
        assert!(
            cut.contains("[... 30100 characters omitted"),
            "{}",
            &cut[OUTPUT_HEAD..OUTPUT_HEAD + 80]
        );
        assert_eq!(truncate_middle("short"), "short");
    }
}
