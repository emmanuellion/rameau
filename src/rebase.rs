//! Interactive rebase editor: reorder commits, reword, squash, fixup or drop them,
//! then hand the resulting todo list to `git rebase -i`.

use std::path::{Path, PathBuf};

use git2::Oid;
use gpui::{App, ClickEvent, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, prelude::*, px};
use gpui_component::{
    Disableable, IconName,
    input::{Input, InputState},
};

use crate::theme::{alpha, c, mono, pal};
use crate::ui::icon_button;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Pick,
    Reword,
    Squash,
    Fixup,
    Drop,
}

impl Action {
    fn next(self) -> Self {
        match self {
            Self::Pick => Self::Reword,
            Self::Reword => Self::Squash,
            Self::Squash => Self::Fixup,
            Self::Fixup => Self::Drop,
            Self::Drop => Self::Pick,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pick => "garder",
            Self::Reword => "reformuler",
            Self::Squash => "fusionner",
            Self::Fixup => "fusionner sans message",
            Self::Drop => "supprimer",
        }
    }

    fn color(self) -> u32 {
        let p = pal();
        match self {
            Self::Pick => p.muted,
            Self::Reword => p.accent,
            Self::Squash | Self::Fixup => p.yellow,
            Self::Drop => p.red,
        }
    }
}

struct Step {
    id: Oid,
    summary: String,
    message: String,
    action: Action,
    input: Option<Entity<InputState>>,
}

pub struct RebaseEditor {
    base: Option<Oid>,
    steps: Vec<Step>,
}

/// Todo list for `git rebase -i`. A reword is a pick followed by an amend reading the new
/// message from a file, so no editor has to be driven.
pub fn todo_text(steps: &[(Action, String, Option<PathBuf>)]) -> Result<String, String> {
    let mut out = String::new();
    let mut has_previous = false;
    for (action, sha, message_file) in steps {
        let verb = match action {
            Action::Pick | Action::Reword => "pick",
            Action::Squash => "squash",
            Action::Fixup => "fixup",
            Action::Drop => "drop",
        };
        if matches!(action, Action::Squash | Action::Fixup) && !has_previous {
            return Err("Le premier commit gardé ne peut pas être fusionné : il n'y a pas de commit au-dessus.".into());
        }
        has_previous |= *action != Action::Drop;
        out.push_str(&format!("{verb} {sha}\n"));
        if let (Action::Reword, Some(file)) = (action, message_file) {
            out.push_str(&format!("exec git commit --amend --only --quiet -F '{}'\n", file.display()));
        }
    }
    Ok(out)
}

impl RebaseEditor {
    pub fn new(base: Option<Oid>, commits: Vec<(Oid, String, String)>) -> Self {
        let steps = commits
            .into_iter()
            .map(|(id, summary, message)| Step { id, summary, message, action: Action::Pick, input: None })
            .collect();
        Self { base, steps }
    }

    /// (base, todo list), writing reworded messages next to `dir`.
    pub fn plan(&self, dir: &Path, cx: &App) -> Result<(Option<Oid>, String), String> {
        let mut steps = Vec::new();
        for (i, s) in self.steps.iter().enumerate() {
            let mut file = None;
            if s.action == Action::Reword {
                let msg = s.input.as_ref().map_or(s.message.clone(), |i| i.read(cx).value().trim().to_owned());
                if msg.is_empty() {
                    return Err(format!("Le message de {} est vide.", &s.id.to_string()[..7]));
                }
                // shortcut: message files stay in the temp dir (an `exec` may run later, after a conflict).
                let path = dir.join(format!("rameau-reword-{}-{i}.txt", std::process::id()));
                std::fs::write(&path, msg).map_err(|e| e.to_string())?;
                file = Some(path);
            }
            steps.push((s.action, s.id.to_string(), file));
        }
        Ok((self.base, todo_text(&steps)?))
    }
}

impl Render for RebaseEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let n = self.steps.len();
        let rows = self.steps.iter().enumerate().map(|(i, s)| {
            let action = s.action;
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .child(
                    icon_button(("up", i), IconName::ArrowUp).disabled(i == 0).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.steps.swap(i, i - 1);
                            cx.notify();
                        },
                    )),
                )
                .child(
                    icon_button(("down", i), IconName::ArrowDown).disabled(i + 1 == n).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.steps.swap(i, i + 1);
                            cx.notify();
                        },
                    )),
                )
                .child(
                    div()
                        .id(("action", i))
                        .w(px(150.))
                        .flex_none()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(c(action.color()))
                        .bg(alpha(action.color(), 0.12))
                        .child(action.label())
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            let step = &mut this.steps[i];
                            step.action = step.action.next();
                            if step.action == Action::Reword && step.input.is_none() {
                                let message = step.message.clone();
                                step.input = Some(cx.new(|cx| {
                                    InputState::new(window, cx).multi_line(true).auto_grow(1, 6).default_value(message)
                                }));
                            }
                            cx.notify();
                        })),
                )
                .child(div().w(px(60.)).flex_none().text_xs().font_family(mono()).text_color(c(p.faint)).child(s.id.to_string()[..7].to_owned()))
                .child(match (&s.input, action) {
                    (Some(input), Action::Reword) => div().flex_1().child(Input::new(input)).into_any_element(),
                    _ => div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(c(if action == Action::Drop { p.faint } else { p.text }))
                        .when(action == Action::Drop, |d| d.line_through())
                        .child(s.summary.clone())
                        .into_any_element(),
                })
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(c(p.muted))
                    .pb_2()
                    .child("Du plus ancien (en haut) au plus récent. Clique sur l'action pour la changer ; « fusionner » réunit le commit avec celui du dessus."),
            )
            .child(div().id("rebase-steps").max_h(px(420.)).overflow_y_scroll().children(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_list() {
        let steps = vec![
            (Action::Pick, "aaa".to_owned(), None),
            (Action::Reword, "bbb".to_owned(), Some(PathBuf::from("/tmp/m.txt"))),
            (Action::Squash, "ccc".to_owned(), None),
            (Action::Drop, "ddd".to_owned(), None),
        ];
        assert_eq!(
            todo_text(&steps).unwrap(),
            "pick aaa\npick bbb\nexec git commit --amend --only --quiet -F '/tmp/m.txt'\nsquash ccc\ndrop ddd\n"
        );
        let bad = vec![(Action::Drop, "a".to_owned(), None), (Action::Fixup, "b".to_owned(), None)];
        assert!(todo_text(&bad).is_err());
    }
}
