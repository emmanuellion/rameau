//! Resolving the conflicts of a file one block at a time: the text between git's markers
//! (`<<<<<<<`, optional `|||||||` base, `=======`, `>>>>>>>`) is replaced by the side picked.

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pick {
    /// The HEAD side, above `=======`.
    Ours,
    /// The incoming side, below `=======`.
    Theirs,
    /// Both, HEAD's first.
    Both,
}

/// `line` is a git conflict marker of this kind: 7 signs, then a space or the end of the line.
pub fn is_marker(line: &str, sign: char) -> bool {
    let rest = line.trim_end_matches(['\n', '\r']);
    rest.len() >= 7 && rest.chars().take(7).all(|c| c == sign) && rest[7..].chars().next().is_none_or(|c| c == ' ')
}

/// The label git wrote after a marker: `HEAD`, a branch, `abc1234 (message)`, `parent of abc1234 (…)`.
pub fn label(line: &str) -> Option<&str> {
    Some(line.get(7..)?.trim()).filter(|l| !l.is_empty())
}

/// The text with the conflict block starting at line `start` (1-based, its `<<<<<<<` line)
/// replaced by the side picked. None when that line is no longer a conflict start or the
/// block has no end: the file changed since it was shown.
pub fn resolve(text: &str, start: usize, pick: Pick) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let first = start.checked_sub(1)?;
    if !is_marker(lines.get(first)?, '<') {
        return None;
    }
    let (mut ours, mut theirs) = (Vec::new(), Vec::new());
    // 0: HEAD side, 1: base (diff3, dropped), 2: incoming side.
    let mut part = 0;
    let mut end = None;
    for (i, l) in lines.iter().enumerate().skip(first + 1) {
        match part {
            0 | 1 if is_marker(l, '|') => part = 1,
            0 | 1 if is_marker(l, '=') => part = 2,
            2 if is_marker(l, '>') => {
                end = Some(i);
                break;
            }
            // Nested start: the block was never closed.
            _ if is_marker(l, '<') => return None,
            0 => ours.push(*l),
            1 => {}
            _ => theirs.push(*l),
        }
    }
    let end = end?;
    // Every kept line is followed by a marker line, so it ends with its newline.
    let kept = match pick {
        Pick::Ours => ours,
        Pick::Theirs => theirs,
        Pick::Both => [ours, theirs].concat(),
    };
    Some(lines[..first].iter().chain(&kept).chain(&lines[end + 1..]).copied().collect())
}

/// Some conflict block is left in the text.
pub fn has_conflicts(text: &str) -> bool {
    text.split_inclusive('\n').any(|l| is_marker(l, '<'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "a\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> feature\nb\n<<<<<<< HEAD\nx\n||||||| base\no\n=======\ny\n>>>>>>> feature\n";

    #[test]
    fn each_block_takes_the_side_picked() {
        assert_eq!(resolve(FILE, 2, Pick::Ours).unwrap(), "a\nmine\nb\n<<<<<<< HEAD\nx\n||||||| base\no\n=======\ny\n>>>>>>> feature\n");
        assert_eq!(resolve(FILE, 2, Pick::Theirs).unwrap(), "a\ntheirs\nb\n<<<<<<< HEAD\nx\n||||||| base\no\n=======\ny\n>>>>>>> feature\n");
        // diff3 style: the base is dropped.
        let both = resolve(FILE, 8, Pick::Both).unwrap();
        assert_eq!(both, "a\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> feature\nb\nx\ny\n");
        assert!(has_conflicts(&both));
        assert!(!has_conflicts(&resolve(&both, 2, Pick::Theirs).unwrap()));
    }

    #[test]
    fn a_stale_line_is_refused() {
        assert_eq!(resolve(FILE, 1, Pick::Ours), None);
        assert_eq!(resolve(FILE, 99, Pick::Ours), None);
        assert_eq!(resolve("<<<<<<< HEAD\nmine\n=======\nno end\n", 1, Pick::Ours), None);
    }

    #[test]
    fn windows_line_ends_and_a_missing_last_newline_survive() {
        let crlf = "<<<<<<< HEAD\r\nmine\r\n=======\r\ntheirs\r\n>>>>>>> f\r\n";
        assert_eq!(resolve(crlf, 1, Pick::Both).unwrap(), "mine\r\ntheirs\r\n");
        let no_eol = "<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> f";
        assert_eq!(resolve(no_eol, 1, Pick::Theirs).unwrap(), "theirs\n");
    }

    #[test]
    fn markers_need_seven_signs_then_a_space() {
        assert!(is_marker("<<<<<<< HEAD\n", '<'));
        assert!(is_marker("=======\r\n", '='));
        assert!(!is_marker("========\n", '='));
        assert!(!is_marker("<<<<<<<x\n", '<'));
        assert_eq!(label(">>>>>>> abc1234 (message)"), Some("abc1234 (message)"));
        assert_eq!(label("======="), None);
    }
}
