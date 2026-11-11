//! Partial patches: keep only the selected +/- lines of a file diff, so `git apply`
//! can stage, unstage or discard just those lines.

use crate::repo::{DiffLine, LineKind};

/// Builds a patch for `path` from the lines where `selected(i)` is true.
/// `reverse`: the patch will be applied with `--reverse` (unstaging from the staged diff).
/// Unselected lines are turned into context where the target file still has them,
/// dropped otherwise. Counts in hunk headers are left to `git apply --recount`.
pub fn build(path: &str, lines: &[DiffLine], selected: impl Fn(usize) -> bool, reverse: bool) -> Option<String> {
    let mut out = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    let mut any = false;
    let mut i = 0;
    while i < lines.len() {
        let LineKind::Hunk = lines[i].kind else {
            i += 1;
            continue;
        };
        let (old_start, new_start) = (lines[i].old.unwrap_or(1), lines[i].new.unwrap_or(1));
        let mut body = String::new();
        let mut changed = false;
        // Whether the previous diff line made it into the patch: its "no newline" marker follows it.
        let mut previous_kept = false;
        i += 1;
        while i < lines.len() && lines[i].kind != LineKind::Hunk {
            let l = &lines[i];
            if l.kind == LineKind::NoNewline {
                if previous_kept {
                    body.push_str("\\ No newline at end of file\n");
                }
                i += 1;
                continue;
            }
            let keep = selected(i);
            let prefix = match (l.kind, keep, reverse) {
                (LineKind::Context, _, _) => Some(' '),
                (LineKind::Added, true, _) => Some('+'),
                (LineKind::Removed, true, _) => Some('-'),
                // The file the patch applies to has unselected removed lines (forward) or
                // unselected added lines (reverse): they become context.
                (LineKind::Removed, false, false) | (LineKind::Added, false, true) => Some(' '),
                _ => None,
            };
            previous_kept = prefix.is_some();
            if let Some(p) = prefix {
                changed |= p != ' ';
                body.push(p);
                body.push_str(&l.text);
                body.push('\n');
            }
            i += 1;
        }
        if changed {
            any = true;
            out.push_str(&format!("@@ -{old_start} +{new_start} @@\n{body}"));
        }
    }
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use crate::repo::{self, DiffSource};
    use std::path::Path;

    fn sh(dir: &Path, args: &[&str]) {
        git::run(dir, args).unwrap();
    }

    fn index_content(dir: &Path) -> String {
        git::run(dir, &["show", ":f"]).unwrap()
    }

    #[test]
    fn stage_and_unstage_single_lines() {
        let dir = std::env::temp_dir().join(format!("rameau-patch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q"]);
        std::fs::write(dir.join("f"), "a\nb\nc\n").unwrap();
        sh(&dir, &["add", "f"]);
        sh(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        std::fs::write(dir.join("f"), "a\nB\nc\nd\n").unwrap();
        let repo = git2::Repository::open(&dir).unwrap();

        // Unstaged diff: -b +B +d. Stage only "+d".
        let (lines, partial) = repo::file_diff(&repo, &DiffSource::Unstaged, "f").unwrap();
        assert!(partial);
        let d = lines.iter().position(|l| l.kind == LineKind::Added && l.text == "d").unwrap();
        let p = build("f", &lines, |i| i == d, false).unwrap();
        git::exec(&dir, &["apply", "--cached", "--recount", "-"], Some(&p), None, &[]).unwrap();
        assert_eq!(index_content(&dir), "a\nb\nc\nd");

        // Stage everything, then unstage only "+d" from the staged diff.
        sh(&dir, &["add", "f"]);
        let (lines, _) = repo::file_diff(&repo, &DiffSource::Staged, "f").unwrap();
        let d = lines.iter().position(|l| l.kind == LineKind::Added && l.text == "d").unwrap();
        let p = build("f", &lines, |i| i == d, true).unwrap();
        git::exec(&dir, &["apply", "--cached", "--recount", "--reverse", "-"], Some(&p), None, &[]).unwrap();
        assert_eq!(index_content(&dir), "a\nB\nc");

        // Nothing selected: no patch.
        assert!(build("f", &lines, |_| false, true).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn last_line_without_newline() {
        let dir = std::env::temp_dir().join(format!("rameau-patch-eof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q"]);
        std::fs::write(dir.join("f"), "a\nb").unwrap();
        sh(&dir, &["add", "f"]);
        sh(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        std::fs::write(dir.join("f"), "a\nB").unwrap();
        let repo = git2::Repository::open(&dir).unwrap();

        // Stage every changed line: the index must then match the file exactly.
        let (lines, _) = repo::file_diff(&repo, &DiffSource::Unstaged, "f").unwrap();
        let changed = |i: usize| matches!(lines[i].kind, LineKind::Added | LineKind::Removed);
        let p = build("f", &lines, changed, false).unwrap();
        git::exec(&dir, &["apply", "--cached", "--recount", "-"], Some(&p), None, &[]).unwrap();
        assert_eq!(git::run(&dir, &["diff", "--stat"]).unwrap(), "", "index differs from the file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
