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
use crate::repo::{self, DiffLine, DiffSource, LineKind};
use crate::theme::{self, alpha, c, mono, pal};
use crate::ui::{appear, button_sm, key};

const LINE_H: f32 = 22.;
// No syntax colors over 1 MB: on such files (generated, minified) colors cost more time than they help.
const MAX_HIGHLIGHT_BYTES: usize = 1 << 20;

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
        text.filter(|t| t.len() <= MAX_HIGHLIGHT_BYTES).map(|t| {
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
            .px_4()
            .border_b_1()
            .border_color(c(p.border))
            .child(div().min_w_0().truncate().text_sm().font_family(mono()).text_color(c(p.text)).child(d.path.clone()))
            .child(div().flex_none().text_xs().text_color(c(p.faint)).child(source))
            .child(div().flex_1())
            .when(d.selectable && n == 0, |el| {
                el.child(div().text_xs().text_color(c(p.faint)).child("Clique sur des lignes pour les choisir (Maj+clic : plage)"))
            })
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
            .child(button_sm("close-diff").icon(IconName::Close).label("Fermer").ghost().on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.close_diff();
                    cx.notify();
                },
            )));

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(appear(
                uniform_list(
                    "diff",
                    d.lines.len(),
                    cx.processor(|this, range: Range<usize>, _, cx| {
                        let Some(d) = &this.diff else { return vec![] };
                        range.map(|ix| diff_line(d, ix, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.diff_scroll.clone())
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .with_width_from_item(Some(d.longest))
                .flex_1()
                .font_family(mono())
                .text_xs(),
                // Replays when another file (or side) is opened, not when lines get staged.
                ("diff-in", key((&d.path, d.source == DiffSource::Staged, matches!(d.source, DiffSource::Commit(_))))),
                4.,
                0.,
            ))
    }
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
        LineKind::Context => (None, "", p.faint),
    };
    let selected = d.selected.contains(&ix);
    let changed = matches!(l.kind, LineKind::Added | LineKind::Removed);
    let staged = d.source == DiffSource::Staged;
    let (text, styles) = expand_tabs(&l.text, &d.highlights[ix]);
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
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_move_highlights() {
        let s = HighlightStyle::default();
        // "\tfoo" with "foo" highlighted at 1..4 → "    foo" with 4..7.
        let (text, styles) = expand_tabs("\tfoo\r", &vec![(1..4, s)]);
        assert_eq!(text, "    foo");
        assert_eq!(styles[0].0, 4..7);
    }
}
