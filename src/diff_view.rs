//! Diff view: syntax-highlighted lines, horizontal scroll, and line/hunk selection
//! to stage, unstage or discard part of a file.

use std::ops::Range;
use std::path::Path;

use git2::Repository;
use gpui::{
    AnyElement, ClickEvent, Context, HighlightStyle, IntoElement, ListHorizontalSizingBehavior, ParentElement, Styled,
    StyledText, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    IconName, Rope, Sizable,
    button::{Button, ButtonVariants},
    highlighter::{HighlightTheme, Language, SyntaxHighlighter},
};

use crate::app::{LineOp, OpenDiff, Rameau};
use crate::conflict::{self, Pick};
use crate::repo::{self, DiffLine, DiffSource, LineKind};
use crate::theme::{self, alpha, c, mono, pal};
use crate::ui::{appear, button_sm, icon_button, key};

const LINE_H: f32 = 22.;

type Styles = Vec<(Range<usize>, HighlightStyle)>;

/// Syntax colors per diff line. Both versions of the file are highlighted whole (so strings
/// and comments spanning lines are right), then each diff line takes its slice.
pub fn highlight(repo: &Repository, source: &DiffSource, path: &str, lines: &[DiffLine]) -> Vec<Styles> {
    let p = Path::new(path);
    let key = p.extension().or(p.file_name()).and_then(|s| s.to_str()).unwrap_or("");
    let lang = Language::from_str(key);
    if matches!(lang, Language::Plain) {
        return vec![Vec::new(); lines.len()];
    }
    let theme = if theme::is_light() { HighlightTheme::default_light() } else { HighlightTheme::default_dark() };
    let side = |text: Option<String>| {
        text.filter(|t| t.len() <= repo::MAX_HIGHLIGHT_BYTES).map(|t| {
            let mut h = SyntaxHighlighter::new(lang.name());
            h.update(None, &Rope::from(t.as_str()));
            let starts: Vec<usize> = std::iter::once(0).chain(t.match_indices('\n').map(|(i, _)| i + 1)).collect();
            (h, t, starts)
        })
    };
    let (old, new) = repo::file_versions(repo, source, path);
    let (old, new) = (side(old), side(new));
    lines
        .iter()
        .map(|l| {
            let (side, n) = match l.kind {
                LineKind::Removed => (&old, l.old),
                LineKind::Added | LineKind::Context => (&new, l.new),
                _ => return Vec::new(),
            };
            let (Some((h, text, starts)), Some(n)) = (side, n) else { return Vec::new() };
            let Some(&start) = starts.get(n as usize - 1) else { return Vec::new() };
            let end = starts.get(n as usize).map_or(text.len(), |e| e - 1);
            // The file on disk may differ from the diff (filters, line endings): no colors then.
            if text[start..end] != l.text {
                return Vec::new();
            }
            h.styles(&(start..end), &theme)
                .into_iter()
                .filter(|(r, _)| r.start < r.end)
                .map(|(r, s)| (r.start - start..r.end - start, s))
                .collect()
        })
        .collect()
}

/// A line of the side-by-side diff: a hunk header or message across both sides, or a line on each
/// side (old on the left, new on the right; a context line sits on both).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SplitRow {
    Full(usize),
    Pair(Option<usize>, Option<usize>),
}

/// Side-by-side rows. In each block of changes, a removed line faces the most alike added line
/// (kept in order); lines with no look-alike face each other in order, or a blank.
pub fn split_rows(lines: &[DiffLine]) -> Vec<SplitRow> {
    let mut rows = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            LineKind::Context => {
                rows.push(SplitRow::Pair(Some(i), Some(i)));
                i += 1;
            }
            LineKind::Removed | LineKind::Added => {
                let run = |from: usize, kind| (from..lines.len()).take_while(|&j| lines[j].kind == kind).collect::<Vec<_>>();
                let removed = run(i, LineKind::Removed);
                let added = run(i + removed.len(), LineKind::Added);
                i += removed.len() + added.len();
                // Lines waiting for a look-alike, then paired in order with the added lines skipped.
                let flush = |rows: &mut Vec<SplitRow>, left: &mut Vec<usize>, right: &[usize]| {
                    for k in 0..left.len().max(right.len()) {
                        rows.push(SplitRow::Pair(left.get(k).copied(), right.get(k).copied()));
                    }
                    left.clear();
                };
                let (mut waiting, mut next) = (Vec::new(), 0);
                for r in removed {
                    let best = added[next..]
                        .iter()
                        .enumerate()
                        .filter_map(|(k, &a)| changed_span(&lines[r].text, &lines[a].text).map(|(old, _)| (lines[r].text.len() - old.len(), k)))
                        .max_by_key(|&(common, k)| (common, std::cmp::Reverse(k)));
                    match best {
                        Some((_, k)) => {
                            flush(&mut rows, &mut waiting, &added[next..next + k]);
                            rows.push(SplitRow::Pair(Some(r), Some(added[next + k])));
                            next += k + 1;
                        }
                        None => waiting.push(r),
                    }
                }
                flush(&mut rows, &mut waiting, &added[next..]);
            }
            _ => {
                rows.push(SplitRow::Full(i));
                i += 1;
            }
        }
    }
    rows
}

/// What changed inside a line edited in place: the differing middle of the old and of the new
/// text, between what they share at the start and at the end. None when the lines aren't alike
/// (less than half of the shorter one in common, indentation aside): the whole line changed.
pub fn changed_span(old: &str, new: &str) -> Option<(Range<usize>, Range<usize>)> {
    let prefix: usize = old.chars().zip(new.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum();
    let (o, n) = (&old[prefix..], &new[prefix..]);
    let suffix: usize = o.chars().rev().zip(n.chars().rev()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum();
    let indent = old[..prefix].len() - old[..prefix].trim_start().len();
    let shorter = old.trim().len().min(new.trim().len());
    if (prefix + suffix - indent) * 2 < shorter.max(1) {
        return None;
    }
    Some((prefix..old.len() - suffix, prefix..new.len() - suffix))
}

/// Per line, the part that changed inside it (only lines facing another one in the side-by-side
/// rows): drawn stronger than the rest of the line.
pub fn word_changes(lines: &[DiffLine], rows: &[SplitRow]) -> Vec<Option<Range<usize>>> {
    let mut out = vec![None; lines.len()];
    for row in rows {
        if let SplitRow::Pair(Some(r), Some(a)) = *row
            && r != a
            && let Some((old, new)) = changed_span(&lines[r].text, &lines[a].text)
        {
            out[r] = Some(old);
            out[a] = Some(new);
        }
    }
    out
}

/// Display text (tabs as 4 spaces, no `\r`) with highlight ranges moved accordingly.
fn expand_tabs(raw: &str, styles: &Styles) -> (String, Styles) {
    if !raw.contains(['\t', '\r']) {
        return (raw.to_owned(), styles.clone());
    }
    let mut out = String::with_capacity(raw.len() + 8);
    let mut map = vec![0; raw.len() + 1];
    for (i, ch) in raw.char_indices() {
        map[i..i + ch.len_utf8()].fill(out.len());
        match ch {
            '\t' => out.push_str("    "),
            '\r' => {}
            ch => out.push(ch),
        }
    }
    map[raw.len()] = out.len();
    let styles = styles
        .iter()
        .filter(|(r, _)| r.end <= raw.len())
        .map(|(r, s)| (map[r.start]..map[r.end], *s))
        .filter(|(r, _)| r.start < r.end)
        .collect();
    (out, styles)
}

impl Rameau {
    pub fn toggle_line(&mut self, ix: usize, extend: bool, cx: &mut Context<Self>) {
        let Some(d) = &mut self.diff else { return };
        let (lines, selected) = (&d.lines, &mut d.selected);
        let changed = |i: usize| matches!(lines[i].kind, LineKind::Added | LineKind::Removed);
        match (extend, d.anchor) {
            (true, Some(a)) => selected.extend((a.min(ix)..=a.max(ix)).filter(|&i| changed(i))),
            _ => {
                if !selected.remove(&ix) {
                    selected.insert(ix);
                }
                d.anchor = Some(ix);
            }
        }
        cx.notify();
    }

    pub fn diff_view(&self, d: &OpenDiff, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let staged = d.source == DiffSource::Staged;
        let source = match &d.source {
            DiffSource::Commit(id) => format!("commit {}", &id.to_string()[..7]),
            DiffSource::Staged => "dans le commit".into(),
            DiffSource::Unstaged => "non indexé".into(),
        };
        let n = d.selected.len();
        let split = self.config.split_diff;
        // Partly checked file: switch between the part going into the commit and the rest.
        let partial = !matches!(d.source, DiffSource::Commit(_))
            && self.history().is_some_and(|h| h.changes.files.iter().any(|f| f.path == d.path && f.check == repo::Check::Part));
        let conflicted = d.source == DiffSource::Unstaged && self.history().is_some_and(|h| h.changes.conflicted.contains(&d.path));
        let other = if staged { DiffSource::Unstaged } else { DiffSource::Staged };
        let other_path = d.path.clone();
        let button = |id: &'static str, label: String, op: LineOp, cx: &mut Context<Self>| {
            button_sm(id).label(label).when(op == LineOp::Discard, |b| b.ghost()).when(op != LineOp::Discard, |b| b.primary()).on_click(
                cx.listener(move |this, _: &ClickEvent, window, cx| this.apply_lines(op, None, window, cx)),
            )
        };
        let header = div()
            .h(px(36.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .pl_2()
            .pr_4()
            .border_b_1()
            .border_color(c(p.border))
            // First: whatever doesn't fit at the right end of the header, closing stays at hand.
            .child(icon_button("close-diff", IconName::Close).flex_none().tooltip("Fermer (Échap)").on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.close_diff();
                    cx.notify();
                },
            )))
            .child(div().min_w_0().truncate().text_sm().font_family(mono()).text_color(c(p.text)).child(d.path.clone()))
            .child(div().flex_none().text_xs().text_color(c(p.faint)).child(source))
            .when(partial, |el| {
                let label = if staged { "Voir la partie hors commit" } else { "Voir la partie dans le commit" };
                el.child(button_sm("other-side").ghost().label(label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.open_diff(other.clone(), other_path.clone(), false, cx)
                })))
            })
            .child(
                div()
                    .flex_none()
                    .flex()
                    .p_0p5()
                    .rounded(px(7.))
                    .bg(c(p.hover))
                    .child(layout_button("unified", "Unifié", !split, cx))
                    .child(layout_button("split", "Côte à côte", split, cx)),
            )
            .when(conflicted, |el| {
                let whole = |id: &'static str, label: &'static str, theirs: bool, cx: &mut Context<Self>| {
                    let path = d.path.clone();
                    button_sm(id).ghost().label(label).tooltip("Tout le fichier prend cette version, et il est marqué résolu").on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| this.resolve_file(path.clone(), theirs, window, cx)),
                    )
                };
                el.child(whole("all-ours", "Tout HEAD", false, cx)).child(whole("all-theirs", "Tout entrant", true, cx))
            })
            // The hint gives way first when the header gets narrow: the buttons stay.
            .child(div().flex_1().min_w_0().flex().justify_end().when(d.selectable && n == 0, |el| {
                el.child(div().min_w_0().truncate().text_xs().text_color(c(p.faint)).child("Clique sur des lignes pour les choisir (Maj+clic : plage)"))
            }))
            .when(d.selectable && n > 0, |el| {
                let lines = if n == 1 { "1 ligne".to_owned() } else { format!("{n} lignes") };
                let main = if staged {
                    button("unstage-lines", format!("Retirer {lines} du commit"), LineOp::Unstage, cx)
                } else {
                    button("stage-lines", format!("Ajouter {lines} au commit"), LineOp::Stage, cx)
                };
                el.child(main)
                    .when(!staged, |el| el.child(button("discard-lines", "Abandonner ces lignes…".into(), LineOp::Discard, cx)))
                    .child(button_sm("unselect").label("Désélectionner").ghost().on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            if let Some(d) = &mut this.diff {
                                d.selected.clear();
                            }
                            cx.notify();
                        },
                    )))
            })
            ;

        // As wide as the room it gets, whatever the longest line: the header's buttons stay visible.
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(header)
            .child(appear(
                if split {
                    // Each side clips its long lines: the two must stay aligned.
                    uniform_list(
                        "diff-split",
                        d.rows.len(),
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            let Some(d) = &this.diff else { return vec![] };
                            range.map(|ix| split_row(d, ix, cx)).collect::<Vec<_>>()
                        }),
                    )
                } else {
                    uniform_list(
                        "diff",
                        d.lines.len(),
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            let Some(d) = &this.diff else { return vec![] };
                            range.map(|ix| diff_line(d, ix, cx)).collect::<Vec<_>>()
                        }),
                    )
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .with_width_from_item(Some(d.longest))
                }
                .track_scroll(self.diff_scroll.clone())
                .flex_1()
                .font_family(mono())
                .text_xs(),
                // Replays when another file (or side) is opened, not when lines get staged.
                ("diff-in", key((&d.path, d.source == DiffSource::Staged, matches!(d.source, DiffSource::Commit(_)), split))),
                4.,
                0.,
            ))
    }
}

/// "Unifié" / "Côte à côte" switch of the diff header.
fn layout_button(id: &'static str, label: &'static str, active: bool, cx: &mut Context<Rameau>) -> impl IntoElement {
    let p = pal();
    div()
        .id(id)
        .px_2()
        .h(px(22.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .text_xs()
        .cursor_pointer()
        .when(active, |d| d.bg(c(p.bg)).text_color(c(p.text)))
        .when(!active, |d| d.text_color(c(p.muted)).hover(|s| s.text_color(c(p.text))))
        .child(label)
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if !active {
                this.toggle_split(cx);
            }
        }))
}

/// Display text of a line with its syntax colors, and the words that changed inside it on a
/// stronger background.
fn line_text(d: &OpenDiff, ix: usize) -> (String, Styles) {
    let p = pal();
    let l = &d.lines[ix];
    let word = d.words[ix].clone().filter(|r| !r.is_empty()).map(|r| {
        let color = if l.kind == LineKind::Removed { p.red } else { p.green };
        (r, HighlightStyle { background_color: Some(alpha(color, 0.38).into()), ..Default::default() })
    });
    let styles: Styles = match word {
        Some(w) => gpui::combine_highlights(d.highlights[ix].clone(), [w]).collect(),
        None => d.highlights[ix].clone(),
    };
    expand_tabs(&l.text, &styles)
}

/// A row of the side-by-side diff.
fn split_row(d: &OpenDiff, ix: usize, cx: &mut Context<Rameau>) -> AnyElement {
    let p = pal();
    match d.rows[ix] {
        SplitRow::Full(i) => diff_line(d, i, cx),
        SplitRow::Pair(left, right) => div()
            .h(px(LINE_H))
            .w_full()
            .flex()
            .child(split_half(d, left, true, cx))
            .child(div().w(px(1.)).h_full().flex_none().bg(c(p.border)))
            .child(split_half(d, right, false, cx))
            .into_any_element(),
    }
}

/// One side of a side-by-side row: the old version on the left, the new one on the right.
fn split_half(d: &OpenDiff, line: Option<usize>, left: bool, cx: &mut Context<Rameau>) -> AnyElement {
    let p = pal();
    let half = div().flex_1().min_w_0().h_full().overflow_hidden().flex().items_center().whitespace_nowrap();
    let Some(ix) = line else {
        // Nothing on this side: a faint filler keeps both sides aligned.
        return half.bg(alpha(p.border, 0.25)).into_any_element();
    };
    let l = &d.lines[ix];
    let (bg, color) = match l.kind {
        LineKind::Added => (Some(alpha(p.green, 0.13)), p.green),
        LineKind::Removed => (Some(alpha(p.red, 0.13)), p.red),
        _ => (None, p.faint),
    };
    let changed = matches!(l.kind, LineKind::Added | LineKind::Removed);
    let selected = d.selected.contains(&ix);
    let (text, styles) = line_text(d, ix);
    let n = if left { l.old } else { l.new };
    half.id((if left { "l" } else { "r" }, ix))
        .when_some(bg, |d, bg| d.bg(bg))
        .when(selected, |d| d.bg(alpha(p.accent, 0.28)))
        .child(div().w(px(44.)).flex_none().pr_2().flex().justify_end().text_color(c(p.faint)).child(n.map_or(String::new(), |n| n.to_string())))
        .child(div().w(px(14.)).flex_none().text_color(c(color)).child(match (selected, l.kind) {
            (true, _) => "●",
            (_, LineKind::Added) => "+",
            (_, LineKind::Removed) => "-",
            _ => "",
        }))
        .child(div().pr_2().text_color(c(p.text)).child(StyledText::new(text).with_highlights(styles)))
        .when(d.selectable && changed, |row| {
            row.cursor_pointer()
                .hover(|s| s.bg(alpha(p.accent, 0.12)))
                .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| this.toggle_line(ix, e.modifiers().shift, cx)))
        })
        .into_any_element()
}

/// Compact button fitting inside a diff line (lists need rows of equal height).
fn hunk_button(id: impl Into<gpui::ElementId>) -> Button {
    Button::new(id).cursor_pointer().ghost().xsmall().h(px(18.)).px_2().ml_2().rounded(px(5.))
}

fn diff_line(d: &OpenDiff, ix: usize, cx: &mut Context<Rameau>) -> AnyElement {
    let p = pal();
    let l = &d.lines[ix];
    let (bg, marker, color) = match l.kind {
        LineKind::Added => (Some(alpha(p.green, 0.13)), "+", p.green),
        LineKind::Removed => (Some(alpha(p.red, 0.13)), "-", p.red),
        LineKind::Hunk => (Some(alpha(p.accent, 0.08)), "", p.accent),
        LineKind::Info | LineKind::NoNewline => (None, "", p.muted),
        LineKind::Marker => (Some(alpha(p.yellow, 0.22)), "!", p.yellow),
        LineKind::Context => (None, "", p.faint),
    };
    let selected = d.selected.contains(&ix);
    let changed = matches!(l.kind, LineKind::Added | LineKind::Removed);
    let staged = d.source == DiffSource::Staged;
    let (text, styles) = line_text(d, ix);
    let num = |n: Option<u32>| {
        let shown = if l.kind == LineKind::Hunk { String::new() } else { n.map_or(String::new(), |n| n.to_string()) };
        div().w(px(44.)).flex_none().pr_2().flex().justify_end().text_color(c(p.faint)).child(shown)
    };
    div()
        .id(ix)
        .h(px(LINE_H))
        .w_full()
        .flex()
        .items_center()
        .whitespace_nowrap()
        .when_some(bg, |d, bg| d.bg(bg))
        .when(selected, |d| d.bg(alpha(p.accent, 0.28)))
        .child(num(l.old))
        .child(num(l.new))
        .child(
            div()
                .w(px(16.))
                .flex_none()
                .text_color(c(color))
                .child(if selected { "●" } else { marker }),
        )
        .child(
            div()
                .pr_4()
                .text_color(c(if l.kind == LineKind::Hunk { p.accent } else { p.text }))
                .child(StyledText::new(text).with_highlights(styles)),
        )
        .when(l.kind == LineKind::Hunk && d.selectable, |row| {
            let op = if staged { LineOp::Unstage } else { LineOp::Stage };
            let label = if staged { "Retirer le bloc" } else { "Ajouter le bloc" };
            row.child(
                hunk_button(("hunk", ix))
                    .label(label)
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.apply_lines(op, Some(ix), window, cx))),
            )
            .when(!staged, |row| {
                row.child(hunk_button(("hunk-discard", ix)).label("Abandonner le bloc…").on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.apply_lines(LineOp::Discard, Some(ix), window, cx),
                )))
            })
        })
        .when(d.selectable && changed, |row| {
            row.cursor_pointer()
                .hover(|s| s.bg(alpha(p.accent, 0.12)))
                .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| this.toggle_line(ix, e.modifiers().shift, cx)))
        })
        .when_some(conflict_start(d, ix), |row, (start, theirs)| {
            let pick = |id: &'static str, label: String, pick: Pick, cx: &mut Context<Rameau>| {
                let path = d.path.clone();
                hunk_button((id, ix)).label(label).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.resolve_block(path.clone(), start, pick, window, cx)))
            };
            let ours = conflict::label(&l.text).unwrap_or("HEAD").to_owned();
            row.child(pick("keep-ours", format!("Garder {}", short_label(&ours)), Pick::Ours, cx))
                .child(pick("keep-theirs", format!("Garder {}", short_label(&theirs)), Pick::Theirs, cx))
                .child(pick("keep-both", "Garder les deux".into(), Pick::Both, cx))
        })
        .into_any_element()
}

/// The start of a conflict block at diff line `ix`: its line in the file and the incoming side's label.
fn conflict_start(d: &OpenDiff, ix: usize) -> Option<(u32, String)> {
    let l = &d.lines[ix];
    if l.kind != repo::LineKind::Marker || !conflict::is_marker(&l.text, '<') {
        return None;
    }
    let end = d.lines[ix + 1..].iter().find(|l| l.kind == LineKind::Marker && conflict::is_marker(&l.text, '>'))?;
    Some((l.new?, conflict::label(&end.text).unwrap_or("entrant").to_owned()))
}

/// A branch name or hash short enough for a button inside a diff line.
fn short_label(s: &str) -> String {
    if s.chars().count() <= 24 { s.to_owned() } else { format!("{}…", s.chars().take(23).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine { kind, old: None, new: None, text: text.into() }
    }

    #[test]
    fn removed_lines_face_added_ones() {
        use LineKind::*;
        let lines = [line(Hunk, "@@"), line(Context, "a"), line(Removed, "b"), line(Removed, "c"), line(Added, "B"), line(Context, "d"), line(Added, "e")];
        assert_eq!(
            split_rows(&lines),
            [
                SplitRow::Full(0),
                SplitRow::Pair(Some(1), Some(1)),
                SplitRow::Pair(Some(2), Some(4)),
                SplitRow::Pair(Some(3), None),
                SplitRow::Pair(Some(5), Some(5)),
                SplitRow::Pair(None, Some(6)),
            ]
        );
        let words = word_changes(&lines, &split_rows(&lines));
        assert_eq!(words[2], None, "b and B share nothing");
    }

    #[test]
    fn only_the_changed_words_stand_out() {
        assert_eq!(changed_span("let x = 1;", "let x = 42;"), Some((8..9, 8..10)));
        // Added in the middle: nothing on the old side, the addition on the new one.
        assert_eq!(changed_span("f(a)", "f(a, b)"), Some((3..3, 3..6)));
        // Accents are cut on character boundaries.
        assert_eq!(changed_span("été", "étés"), Some((5..5, 5..6)));
        assert_eq!(changed_span("    foo", "    bar"), None, "only the indentation in common");
        let lines = [line(LineKind::Removed, "count = 1"), line(LineKind::Added, "count = 2")];
        assert_eq!(word_changes(&lines, &split_rows(&lines)), [Some(8..9), Some(8..9)]);
        assert_eq!(changed_span("println!(\"b\");", "let nom = \"Ada\";"), None, "only `\");` in common");
    }

    #[test]
    fn edited_line_faces_its_new_version() {
        use LineKind::*;
        // A line rewritten, with a new line inserted before its new version.
        let lines = [line(Removed, "    println!(\"b\");"), line(Added, "    let nom = \"Ada\";"), line(Added, "    println!(\"bonjour {}\", nom);")];
        let rows = split_rows(&lines);
        assert_eq!(rows, [SplitRow::Pair(None, Some(1)), SplitRow::Pair(Some(0), Some(2))]);
        let words = word_changes(&lines, &rows);
        assert_eq!((words[0].clone(), words[1].clone()), (Some(15..16), None), "the closing quote moved after the new words");
    }

    #[test]
    fn tabs_move_highlights() {
        let s = HighlightStyle::default();
        // "\tfoo" with "foo" highlighted at 1..4 → "    foo" with 4..7.
        let (text, styles) = expand_tabs("\tfoo\r", &vec![(1..4, s)]);
        assert_eq!(text, "    foo");
        assert_eq!(styles[0].0, 4..7);
    }
}
