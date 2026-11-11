//! Main window layout: title bar, sidebar, history, changes and details panels, context menus.

use git2::RepositoryState;
use gpui::{
    Animation, AnimationExt, MouseMoveEvent, deferred, ease_out_quint, percentage, radians,
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, ExternalPaths, Hsla, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, PathBuilder, Pixels, Point, Render, ScrollStrategy, SharedString, Styled, WeakEntity,
    Window, canvas, div, point, prelude::*, px, quad, size, uniform_list,
};
use gpui_component::{
    Disableable, Icon, IconName, Root, Sizable, TitleBar,
    button::ButtonVariants,
    input::Input,
    menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
};

use crate::app::{CloseDiff, GitJob, Login, MenuTarget, Page, Project, Rameau, SelectNext, SelectPrev, Selection, Side};
use crate::graph::{Half, Row};
use crate::repo::{DiffSource, RefKind, RefLabel};
use crate::theme::{alpha, c, lane_color, mono, pal, status_color};
use crate::ui::{appear, avatar, button, button_sm, icon, icon_button, key};

use std::f32::consts::FRAC_PI_2;
use std::time::Duration;

const ROW_H: f32 = 34.;
const LANE_W: f32 = 16.;
pub const TITLE_BAR_H: f32 = 44.;
const AUTHOR_W: f32 = 170.;
/// Author column reduced to the avatar when the history gets narrow.
const AVATAR_W: f32 = 44.;
const DATE_W: f32 = 110.;
const HASH_W: f32 = 64.;
// shortcut: wider graphs are clipped; make the graph column resizable if repos need it.
const MAX_GRAPH_LANES: usize = 14;

/// Height of every sidebar line (headers included): the list is virtualized.
const SIDE_ROW_H: f32 = 32.;

/// A line of the sidebar: group header (group index, item count), branch or stash index.
#[derive(Clone, Copy)]
enum SideItem {
    Header(usize, usize),
    Branch(usize),
    Stash(usize),
}

struct Group {
    key: &'static str,
    title: &'static str,
    icon: &'static str,
    /// Some(remote?) for branches, None for stashes.
    remote: Option<bool>,
}

const GROUPS: [Group; 3] = [
    Group { key: "branches", title: "Branches", icon: "git-branch", remote: Some(false) },
    Group { key: "remotes", title: "Distantes", icon: "cloud", remote: Some(true) },
    Group { key: "stashes", title: "Stashs", icon: "archive", remote: None },
];

fn project_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Small rounded counter.
fn badge(n: usize, on_accent: bool) -> impl IntoElement {
    let p = pal();
    div()
        .px_1p5()
        .min_w(px(18.))
        .h(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_xs()
        .when(on_accent, |d| d.bg(alpha(p.bg, 0.25)).text_color(c(p.bg)))
        .when(!on_accent, |d| d.bg(c(p.border)).text_color(c(p.muted)))
        .child(n.to_string())
}

/// Which history columns fit in the center's width: the message keeps the room,
/// the hash goes first, then the author's name, then the date.
#[derive(Clone, Copy)]
struct Columns {
    author_name: bool,
    date: bool,
    hash: bool,
}

impl Columns {
    fn for_width(w: f32) -> Self {
        Columns { hash: w >= 780., author_name: w >= 660., date: w >= 540. }
    }

    fn author_w(self) -> f32 {
        if self.author_name { AUTHOR_W } else { AVATAR_W }
    }
}

fn label(text: &'static str) -> impl IntoElement {
    div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(pal().faint)).child(text)
}

impl Rameau {
    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let weak = cx.weak_entity();
        let name = self.proj.as_ref().map_or("Aucun projet".to_owned(), |pr| project_name(&pr.path));
        let switcher = button("project")
            .ghost()
            .icon(IconName::FolderOpen)
            .label(name)
            .child(Icon::new(IconName::ChevronDown).xsmall().text_color(c(p.faint)))
            .dropdown_menu(move |menu, _, cx| Self::projects_menu(&weak, menu, cx));
        let bar = div().flex().items_center().gap_1().w_full().pr_3().child(switcher);
        let Some(pr) = self.proj.as_ref().filter(|_| self.page == Page::Repo) else {
            return TitleBar::new().h(px(TITLE_BAR_H)).child(bar);
        };
        let h = &pr.history;
        let busy = self.busy.is_some();
        let (ahead, behind) = h.ahead_behind.unwrap_or((0, 0));
        let push_primary = ahead > 0 || h.ahead_behind.is_none();
        TitleBar::new().h(px(TITLE_BAR_H)).child(
            bar.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(px(28.))
                    .px_2p5()
                    .rounded(px(7.))
                    .bg(c(p.hover))
                    .text_sm()
                    .child(icon("git-branch").xsmall().text_color(c(p.accent)))
                    .child(h.head.clone()),
            )
            .when(h.head_branch.is_some() && h.ahead_behind.is_none(), |d| {
                d.child(div().pl_1().text_xs().text_color(c(p.faint)).child("pas encore publiée"))
            })
            .child(div().flex_1())
            .children(self.busy.clone().map(|b| {
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .text_xs()
                    .text_color(c(p.muted))
                    .child(
                        Icon::new(IconName::LoaderCircle)
                            .xsmall()
                            .with_animation("busy-spin", Animation::new(Duration::from_millis(900)).repeat(), |icon, t| icon.rotate(percentage(t))),
                    )
                    .child(b)
            }))
            .child(
                button("stash")
                    .ghost()
                    .icon(icon("archive"))
                    .label("Stash")
                    .disabled(busy || h.changes.is_empty())
                    .tooltip("Mettre de côté les modifications locales")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        let job = GitJob::new("Stash", &["stash", "push", "--include-untracked"]).done("Modifications mises de côté");
                        this.run(job, window, cx)
                    })),
            )
            .child(
                button("fetch")
                    .ghost()
                    .icon(icon("refresh-cw"))
                    .label("Fetch")
                    .disabled(busy)
                    .tooltip("Récupérer les nouveautés du serveur sans toucher à tes fichiers")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.run(GitJob::new("Fetch", &["fetch", "--all", "--prune"]), window, cx)
                    })),
            )
            .child(
                button("pull")
                    .ghost()
                    .icon(IconName::ArrowDown)
                    .label("Pull")
                    .when(behind > 0, |b| b.child(badge(behind, false)))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.pull(window, cx)
                    })),
            )
            .child(
                button("push")
                    .icon(IconName::ArrowUp)
                    .label("Push")
                    .when(ahead > 0, |b| b.child(badge(ahead, true)))
                    .when(push_primary, |b| b.primary())
                    .when(!push_primary, |b| b.ghost())
                    .disabled(busy || h.head_branch.is_none())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.push(window, cx))),
            ),
        )
    }

    fn projects_menu(weak: &WeakEntity<Self>, mut menu: PopupMenu, cx: &mut App) -> PopupMenu {
        let Some(this) = weak.upgrade() else { return menu };
        let this = this.read(cx);
        let current = this.dir();
        let projects = this.config.projects.clone();
        for path in projects {
            let name = project_name(&path);
            // Two projects with the same folder name: show where they live.
            let dup = this.config.projects.iter().filter(|p| project_name(p) == name).count() > 1;
            let label = match (dup, path.parent()) {
                (true, Some(parent)) => format!("{name}  ({})", parent.display()),
                _ => name,
            };
            let weak = weak.clone();
            let p = path.clone();
            menu = menu.item(PopupMenuItem::new(label).checked(current.as_ref() == Some(&path)).on_click(move |_, window, cx| {
                weak.update(cx, |this, cx| {
                    this.page = Page::Repo;
                    this.load_project(p.clone(), window, cx)
                })
                .ok();
            }));
        }
        let (w1, w2) = (weak.clone(), weak.clone());
        let current_name = current.as_deref().map(project_name);
        menu.separator()
            .item(PopupMenuItem::new("Importer un projet…").icon(IconName::Plus).on_click(move |_, _, cx| {
                w1.update(cx, |this, cx| {
                    this.page = Page::Import;
                    cx.notify();
                })
                .ok();
            }))
            .when_some(current_name, |menu, name| {
                menu.item(PopupMenuItem::new(format!("Retirer « {name} » de la liste")).on_click(move |_, window, cx| {
                    w2.update(cx, |this, cx| this.remove_current_project(window, cx)).ok();
                }))
            })
    }

    /// Foldable sidebar section header.
    fn section_header(&self, key: &'static str, title: &'static str, ico: Icon, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let open = !self.config.collapsed.iter().any(|k| k == key);
        let header = div()
            .id(key)
            .mx_2()
            .px_2()
            .h(px(28.))
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(7.))
            .cursor_pointer()
            .hover(|s| s.bg(c(p.hover)))
            .child(Icon::new(IconName::ChevronDown).xsmall().text_color(c(p.faint)).with_animation(
                (key, open as usize),
                Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                // Pointing down when open, right when folded; turns between the two.
                move |icon, t| icon.rotate(radians(-FRAC_PI_2 * if open { 1. - t } else { t })),
            ))
            .child(ico.xsmall().text_color(c(p.muted)))
            .child(div().flex_1().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(p.muted)).child(title))
            .child(badge(count, false))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_section(key, cx)));
        header
    }

    /// Sidebar lines, in order: a header per non-empty group, then its rows when unfolded.
    fn side_items(&self, pr: &Project) -> Vec<SideItem> {
        let h = &pr.history;
        let mut items = Vec::new();
        for (g, group) in GROUPS.iter().enumerate() {
            let members: Vec<SideItem> = match group.remote {
                Some(remote) => h
                    .branches
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| (b.kind == RefKind::Remote) == remote)
                    .map(|(i, _)| SideItem::Branch(i))
                    .collect(),
                None => (0..h.stashes.len()).map(SideItem::Stash).collect(),
            };
            if members.is_empty() {
                continue;
            }
            items.push(SideItem::Header(g, members.len()));
            if !self.config.collapsed.iter().any(|k| k == group.key) {
                items.extend(members);
            }
        }
        items
    }

    fn side_item(&self, pr: &Project, item: SideItem, cx: &mut Context<Self>) -> AnyElement {
        let p = pal();
        let h = &pr.history;
        let selected_id = match self.selection {
            Selection::Commit(id) => Some(id),
            _ => None,
        };
        // Every line is SIDE_ROW_H tall: the list only builds the visible ones.
        let line = div().h(px(SIDE_ROW_H)).flex().flex_col().justify_end();
        match item {
            SideItem::Header(g, count) => {
                let group = &GROUPS[g];
                line.child(self.section_header(group.key, group.title, icon(group.icon), count, cx)).into_any_element()
            }
            SideItem::Branch(i) => {
                let b = &h.branches[i];
                let color = b.row.map_or(c(p.faint), |r| lane_color(h.rows[r].color));
                let row = b.row;
                let is_head = b.kind == RefKind::Head;
                let target = MenuTarget::Branch(b.name.clone(), b.kind);
                let selected = row.is_some_and(|r| Some(h.commits[r].id) == selected_id);
                // "origin/feature/x": the remote name fades, the branch stands out.
                let (prefix, name) = match b.kind {
                    RefKind::Remote => b.name.split_once('/').map_or(("", b.name.as_str()), |(r, n)| (r, n)),
                    _ => ("", b.name.as_str()),
                };
                line.child(
                    sidebar_row(("branch", i), selected)
                        .child(div().size(px(8.)).rounded_full().flex_none().bg(color))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .text_sm()
                                .when(!prefix.is_empty(), |d| d.child(div().flex_none().text_color(c(p.faint)).child(format!("{prefix}/"))))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(c(if is_head { p.text } else { p.muted }))
                                        .when(is_head, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
                                        .child(name.to_owned()),
                                ),
                        )
                        .when(is_head, |d| d.child(Icon::new(IconName::Check).xsmall().text_color(c(p.accent))))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_target = Some(target.clone())))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(r) = row {
                                this.scroll.scroll_to_item(r, ScrollStrategy::Center);
                                let id = this.proj.as_ref().unwrap().history.commits[r].id;
                                this.select_commit(id, cx);
                            }
                        })),
                )
                .into_any_element()
            }
            SideItem::Stash(i) => {
                let st = &h.stashes[i];
                let (id, index) = (st.id, st.index);
                line.child(
                    sidebar_row(("stash", index), selected_id == Some(id))
                        .child(div().flex_none().text_xs().font_family(mono()).text_color(c(p.faint)).child(format!("{{{index}}}")))
                        .child(div().flex_1().truncate().text_sm().text_color(c(p.muted)).child(st.message.clone()))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_target = Some(MenuTarget::Stash(index))))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_commit(id, cx))),
                )
                .into_any_element()
            }
        }
    }

    fn sidebar(&self, pr: &Project, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let weak = cx.weak_entity();
        // Virtualized like the history: repositories with thousands of remote branches stay
        // smooth, even while the busy spinner redraws the window every frame.
        let list = uniform_list(
            "sidebar-items",
            self.side_items(pr).len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let Some(pr) = &this.proj else { return vec![] };
                let items = this.side_items(pr);
                range.filter_map(|ix| items.get(ix).map(|item| this.side_item(pr, *item, cx))).collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .pb_4();
        div()
            .id("sidebar")
            .w(px(self.sidebar_w()))
            .flex_none()
            .flex()
            .flex_col()
            .bg(c(p.panel))
            .border_r_1()
            .border_color(c(p.border))
            .child(
                div()
                    .id("branches")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(list)
                    .capture_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, _, _| {
                        if e.button == MouseButton::Right {
                            this.menu_target = None;
                        }
                    }))
                    .context_menu(move |menu, _, cx| Self::build_menu(&weak, menu, cx)),
            )
            .child(self.account(cx))
    }

    fn account(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let theme_button = icon_button("theme", if self.config.light { IconName::Moon } else { IconName::Sun })
            .tooltip(if self.config.light { "Thème sombre" } else { "Thème clair" })
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_theme(window, cx)));
        let body = match &self.login {
            Login::Out => div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    button("login")
                        .outline()
                        .icon(IconName::GitHub)
                        .label("Connexion GitHub")
                        .flex_1()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.login(window, cx))),
                )
                .child(theme_button)
                .into_any_element(),
            Login::Waiting(code) => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_xs().text_color(c(p.muted)).child("Code copié, colle-le sur la page GitHub :"))
                .child(div().text_xl().font_family(mono()).text_color(c(p.text)).child(code.clone()))
                .child(button_sm("cancel-login").ghost().label("Annuler").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.logout(cx))))
                .into_any_element(),
            Login::In(a) => div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(avatar(&a.login, 28.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().text_sm().text_color(c(p.text)).child(a.login.clone()))
                        .child(
                            div()
                                .id("logout")
                                .text_xs()
                                .text_color(c(p.faint))
                                .cursor_pointer()
                                .hover(|s| s.text_color(c(p.accent)))
                                .child("Se déconnecter")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.logout(cx))),
                        ),
                )
                .child(theme_button)
                .into_any_element(),
        };
        div().p_3().border_t_1().border_color(c(p.border)).child(body)
    }

    /// Banner shown while a merge/rebase/cherry-pick waits for the user.
    fn state_banner(&self, pr: &Project, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = pal();
        let (title, cont, abort): (&str, &[&str], &[&str]) = match pr.history.state {
            RepositoryState::Clean => return None,
            RepositoryState::Merge => ("Merge en cours", &["commit", "--no-edit"], &["merge", "--abort"]),
            RepositoryState::Rebase | RepositoryState::RebaseInteractive | RepositoryState::RebaseMerge => {
                ("Rebase en cours", &["rebase", "--continue"], &["rebase", "--abort"])
            }
            RepositoryState::CherryPick | RepositoryState::CherryPickSequence => {
                ("Cherry-pick en cours", &["cherry-pick", "--continue"], &["cherry-pick", "--abort"])
            }
            RepositoryState::Revert | RepositoryState::RevertSequence => ("Revert en cours", &["revert", "--continue"], &["revert", "--abort"]),
            _ => ("Opération git en cours", &["am", "--continue"], &["am", "--abort"]),
        };
        let conflicts = pr.history.changes.conflicted.len();
        let hint = match conflicts {
            0 => "Conflits résolus : tu peux continuer.".to_owned(),
            1 => "1 conflit à résoudre : corrige le fichier puis coche-le.".to_owned(),
            n => format!("{n} conflits à résoudre : corrige les fichiers puis coche-les."),
        };
        let (cont, abort): (Vec<String>, Vec<String>) =
            (cont.iter().map(|s| s.to_string()).collect(), abort.iter().map(|s| s.to_string()).collect());
        Some(
            div()
                .m_3()
                .mb_1()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2p5()
                .rounded(px(10.))
                .bg(alpha(p.yellow, 0.1))
                .border_1()
                .border_color(alpha(p.yellow, 0.35))
                .child(Icon::new(IconName::TriangleAlert).small().text_color(c(p.yellow)))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(p.text)).child(title))
                        .child(div().text_xs().text_color(c(p.muted)).child(hint)),
                )
                .child(button_sm("abort-op").ghost().label("Annuler l'opération").on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.run(GitJob::owned("Annulation", abort.clone()), window, cx),
                )))
                .child(
                    button_sm("continue-op")
                        .primary()
                        .label("Continuer")
                        .disabled(conflicts > 0)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.run(GitJob::owned("Continuer", cont.clone()), window, cx))),
                ),
        )
    }

    fn graph_width(pr: &Project) -> f32 {
        pr.history.max_lanes.min(MAX_GRAPH_LANES) as f32 * LANE_W + 10.
    }

    fn wip_row(&self, pr: &Project, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = pal();
        let ch = &pr.history.changes;
        if ch.is_empty() {
            return None;
        }
        let chip = |n: usize, word: &str, color: u32| {
            (n > 0).then(|| {
                let plural = if n > 1 && word != "en conflit" { "s" } else { "" };
                div().px_2().h(px(20.)).flex().items_center().rounded_full().text_xs().bg(alpha(color, 0.14)).text_color(c(color)).child(format!("{n} {word}{plural}"))
            })
        };
        let selected = self.selection == Selection::Changes;
        Some(
            div()
                .id("wip")
                .h(px(ROW_H))
                .flex_none()
                .flex()
                .items_center()
                .pr_4()
                .cursor_pointer()
                .border_l_2()
                .border_color(if selected { c(p.accent) } else { gpui::transparent_black().into() })
                .when(selected, |d| d.bg(c(p.selected)))
                .when(!selected, |d| d.hover(|s| s.bg(c(p.hover))))
                .on_click(cx.listener(|this, _, _, cx| this.select_changes(cx)))
                .child(
                    div()
                        .w(px(Self::graph_width(pr)))
                        .flex_none()
                        .pl(px(10. + LANE_W / 2. - 6.))
                        .child(div().size(px(12.)).rounded_full().border_2().border_color(c(p.accent)).border_dashed()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_sm().font_weight(gpui::FontWeight::MEDIUM).text_color(c(p.text)).child("Modifications locales"))
                        .children(chip(ch.conflicted.len(), "en conflit", p.red))
                        .children(chip(ch.staged.len(), "indexé", p.green))
                        .children(chip(ch.unstaged.len(), "modifié", p.yellow)),
                ),
        )
    }

    fn commit_row(&self, pr: &Project, ix: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = pal();
        let cols = Columns::for_width(self.center_w);
        let cm = &pr.history.commits[ix];
        let id = cm.id;
        let row = pr.history.rows[ix].clone();
        let color = lane_color(row.color);
        let selected = self.selection == Selection::Commit(id);
        let is_merge = row.segments.iter().filter(|s| s.half == Half::Bottom && s.from == row.lane).count() > 1;
        div()
            .id(ix)
            .h(px(ROW_H))
            .w_full()
            .flex()
            .items_center()
            .pr_4()
            .cursor_pointer()
            .border_l_2()
            .border_color(if selected { c(p.accent) } else { gpui::transparent_black().into() })
            .when(selected, |d| d.bg(c(p.selected)))
            .when(!selected, |d| d.hover(|s| s.bg(c(p.hover))))
            .on_click(cx.listener(move |this, _, _, cx| this.select_commit(id, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.menu_target = Some(MenuTarget::Commit(id));
                    this.select_commit(id, cx);
                }),
            )
            .child(
                canvas(|_, _, _| {}, move |bounds, _, window, _| paint_graph(bounds, &row, is_merge, window))
                    .w(px(Self::graph_width(pr)))
                    .h_full()
                    .flex_none(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    // Pills are clipped at the column edge rather than spilling over the author.
                    .overflow_hidden()
                    .children(cm.refs.iter().map(|r| ref_pill(r, color)))
                    .child(div().min_w_0().truncate().text_sm().text_color(c(p.text)).child(cm.summary.clone())),
            )
            .child(
                div()
                    .w(px(cols.author_w()))
                    .flex_none()
                    .pl_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(avatar(&cm.author, 20.))
                    .when(cols.author_name, |d| d.child(div().min_w_0().truncate().text_xs().text_color(c(p.muted)).child(cm.author.clone()))),
            )
            .when(cols.date, |d| {
                d.child(div().w(px(DATE_W)).flex_none().truncate().text_xs().text_color(c(p.muted)).child(crate::repo::relative_time(cm.time)))
            })
            .when(cols.hash, |d| {
                d.child(div().w(px(HASH_W)).flex_none().text_xs().font_family(mono()).text_color(c(p.faint)).child(cm.id.to_string()[..7].to_owned()))
            })
    }

    fn commit_list(&self, pr: &Project, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let cols = Columns::for_width(self.center_w);
        let weak = cx.weak_entity();
        let col = |text: &'static str| div().text_xs().font_weight(gpui::FontWeight::MEDIUM).text_color(c(p.faint)).child(text);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .pr_4()
                    .border_b_1()
                    .border_color(c(p.border))
                    .child(div().w(px(Self::graph_width(pr) + 2.)).flex_none())
                    .child(div().flex_1().child(col("Message")))
                    .child(div().w(px(cols.author_w())).flex_none().pl_4().child(col(if cols.author_name { "Auteur" } else { "" })))
                    .when(cols.date, |d| d.child(div().w(px(DATE_W)).flex_none().child(col("Date"))))
                    .when(cols.hash, |d| d.child(div().w(px(HASH_W)).flex_none().child(col("Commit")))),
            )
            .children(self.wip_row(pr, cx))
            .child(
                uniform_list(
                    "commits",
                    pr.history.commits.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        let Some(pr) = &this.proj else { return vec![] };
                        range.map(|ix| this.commit_row(pr, ix, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1(),
            )
            .capture_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, _, _| {
                if e.button == MouseButton::Right {
                    this.menu_target = None;
                }
            }))
            .context_menu(move |menu, _, cx| Self::build_menu(&weak, menu, cx))
    }

    fn file_row(
        &self,
        id: (&'static str, usize),
        (status, path): &(char, String),
        source: DiffSource,
        toggle: Option<(bool, Vec<String>)>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = pal();
        let open = self.diff.as_ref().is_some_and(|d| d.path == *path && d.source == source);
        let (dir, name) = path.rsplit_once('/').map_or(("", path.as_str()), |(d, n)| (d, n));
        let pth = path.clone();
        div()
            .id(id)
            .h(px(30.))
            .flex()
            .items_center()
            .gap_2p5()
            .px_2()
            .rounded(px(7.))
            .cursor_pointer()
            .when(open, |d| d.bg(c(p.selected)))
            .when(!open, |d| d.hover(|s| s.bg(c(p.hover))))
            .when_some(toggle, |row, (checked, args)| {
                row.child(
                    div()
                        .id("check")
                        .size(px(16.))
                        .flex_none()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(c(if checked { p.accent } else { p.faint }))
                        .when(checked, |d| d.bg(c(p.accent)))
                        .hover(|s| s.border_color(c(p.accent)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(checked, |d| {
                            d.child(Icon::new(IconName::Check).text_color(c(p.bg)).with_animation(
                                "pop",
                                Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                                |icon, t| icon.size(px(6. + 6. * t)),
                            ))
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.run(GitJob::owned(if checked { "Retrait" } else { "Ajout" }, args.clone()).status_only(), window, cx);
                        })),
                )
            })
            .child(
                div()
                    .size(px(18.))
                    .flex_none()
                    .rounded(px(4.))
                    .bg(gpui::Rgba { a: 0.14, ..status_color(*status) })
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(status_color(*status))
                    .child(status.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap_1p5()
                    .text_sm()
                    .child(div().flex_none().text_color(c(p.text)).child(name.to_owned()))
                    .child(div().min_w_0().truncate().text_xs().pt_0p5().text_color(c(p.faint)).child(dir.to_owned())),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_diff(source.clone(), pth.clone(), false, cx)))
    }

    fn changes_panel(&self, pr: &Project, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let ch = &pr.history.changes;
        let header = |title: &'static str, n: usize, action: Option<(&'static str, &'static str, &'static [&'static str])>, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .pt_5()
                .pb_1p5()
                .px_2()
                .child(label(title))
                .child(badge(n, false))
                .child(div().flex_1())
                .children(action.map(|(id, text, args)| {
                    button_sm(id)
                        .ghost()
                        .label(text)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.run(GitJob::new(text, args).status_only(), window, cx)))
                }))
        };
        let path_args = |cmd: &[&str], path: &str| cmd.iter().map(|s| s.to_string()).chain([path.to_owned()]).collect::<Vec<_>>();
        let mut files = div().id("changes-files").flex_1().overflow_y_scroll().px_3().pt_4().pb_3();
        files = files.child(div().px_2().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(p.text)).child("Modifications locales"));
        if !ch.conflicted.is_empty() {
            files = files.child(header("CONFLITS", ch.conflicted.len(), None, cx));
            for (i, path) in ch.conflicted.iter().enumerate() {
                let entry = ('U', path.clone());
                let a = path_args(&["add", "--"], path);
                files = files.child(self.file_row(("conflict", i), &entry, DiffSource::Unstaged, Some((false, a)), cx));
            }
        }
        if !ch.unstaged.is_empty() {
            files = files.child(header("NON INDEXÉS", ch.unstaged.len(), Some(("stage-all", "Tout cocher", &["add", "-A"])), cx));
            for (i, f) in ch.unstaged.iter().enumerate() {
                let a = path_args(&["add", "--"], &f.1);
                let target = MenuTarget::File(f.1.clone(), f.0 == 'A');
                files = files.child(
                    self.file_row(("unstaged", i), f, DiffSource::Unstaged, Some((false, a)), cx)
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_target = Some(target.clone()))),
                );
            }
        }
        if !ch.staged.is_empty() {
            files = files.child(header("DANS LE COMMIT", ch.staged.len(), Some(("unstage-all", "Tout décocher", &["reset", "-q"])), cx));
            for (i, f) in ch.staged.iter().enumerate() {
                let a = path_args(&["reset", "-q", "--"], &f.1);
                files = files.child(self.file_row(("staged", i), f, DiffSource::Staged, Some((true, a)), cx));
            }
        }
        let n = ch.staged.len();
        let weak = cx.weak_entity();
        let key = if cfg!(target_os = "macos") { "⌘↩" } else { "Ctrl+↩" };
        div()
            .id("changes-panel")
            .w(px(self.panel_w()))
            .flex_none()
            .flex()
            .flex_col()
            .bg(c(p.panel))
            .border_l_1()
            .border_color(c(p.border))
            .child(appear(
                files
                    .capture_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, _, _| {
                        if e.button == MouseButton::Right {
                            this.menu_target = None;
                        }
                    }))
                    .context_menu(move |menu, _, cx| Self::build_menu(&weak, menu, cx)),
                "changes-in",
                6.,
                0.,
            ))
            .child(
                div()
                    .flex_none()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .border_t_1()
                    .border_color(c(p.border))
                    .child(Input::new(&self.message).h(px(100.)))
                    .child(
                        button("commit")
                            .primary()
                            .w_full()
                            .h(px(38.))
                            .icon(icon("git-commit"))
                            .label(match n {
                                0 => "Commit".to_owned(),
                                1 => "Commit 1 fichier".to_owned(),
                                n => format!("Commit {n} fichiers"),
                            })
                            .disabled(n == 0 || self.busy.is_some())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.commit(window, cx))),
                    )
                    .child(div().text_xs().text_color(c(p.faint)).child(format!("{key} pour valider · clic droit sur un fichier pour l'abandonner"))),
            )
    }

    fn details_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = pal();
        let d = self.details.as_ref()?;
        let Selection::Commit(id) = self.selection else { return None };
        let (title, body) = d.message.split_once('\n').unwrap_or((&d.message, ""));
        let files: Vec<_> =
            d.files.iter().enumerate().map(|(i, f)| self.file_row(("file", i), f, DiffSource::Commit(id), None, cx)).collect();
        let hash = d.id.clone();
        let chip = |text: String| {
            div().px_2().py_0p5().rounded(px(6.)).bg(c(p.hover)).text_xs().font_family(mono()).text_color(c(p.muted)).child(text)
        };
        Some(
            div()
                .id("details")
                .w(px(self.panel_w()))
                .flex_none()
                .bg(c(p.panel))
                .border_l_1()
                .border_color(c(p.border))
                .overflow_y_scroll()
                .child(appear(
            div()
                .p_5()
                .flex()
                .flex_col()
                .gap_5()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(p.text)).child(title.to_owned()))
                        .when(!body.trim().is_empty(), |el| el.child(div().text_sm().text_color(c(p.muted)).child(body.trim().to_owned()))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(avatar(&d.author, 34.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(div().truncate().text_sm().font_weight(gpui::FontWeight::MEDIUM).text_color(c(p.text)).child(d.author.clone()))
                                .child(div().truncate().text_xs().text_color(c(p.muted)).child(format!("{} · {}", d.email, d.date))),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(label("COMMIT"))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(chip(d.id[..12].to_owned()))
                                .child(
                                    icon_button("copy-hash", IconName::Copy)
                                        .tooltip("Copier le hash complet")
                                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))),
                                ),
                        )
                        .when(!d.parents.is_empty(), |el| {
                            el.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_xs()
                                    .text_color(c(p.faint))
                                    .child(if d.parents.len() > 1 { "parents" } else { "parent" })
                                    .children(d.parents.iter().map(|h| chip(h.clone()))),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().flex().items_center().gap_2().pb_1().child(label("FICHIERS")).child(badge(d.files.len(), false)))
                        .children(files),
                ),
                // Replays for each commit selected.
                ("details-in", key(id)),
                6.,
                0.,
            )),
        )
    }

    /// Invisible 6px grip on a panel edge: a thin accent line shows on hover and while dragging.
    /// Double-click restores the default width.
    fn resize_handle(&self, side: Side, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let active = self.resizing == Some(side);
        let id = if side == Side::Left { "resize-left" } else { "resize-right" };
        div().w(px(0.)).h_full().relative().child(
            // Painted last so the panels on both sides don't cover the grip.
            deferred(
                div()
                    .id(id)
                    .group(id)
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(-3.))
                    .w(px(6.))
                    .cursor_col_resize()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w(px(2.))
                            .h_full()
                            .when(active, |d| d.bg(c(p.accent)))
                            .group_hover(id, |s| s.bg(alpha(p.accent, 0.6))),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.resizing = Some(side);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                        if e.click_count() == 2 {
                            this.reset_width(side, cx);
                        }
                    })),
            ),
        )
    }

    fn build_menu(weak: &WeakEntity<Self>, menu: PopupMenu, cx: &mut App) -> PopupMenu {
        let Some(this) = weak.upgrade() else { return menu };
        let this = this.read(cx);
        let Some(pr) = &this.proj else { return menu };
        let head = pr.history.head.clone();
        // Item running a git command.
        let git = |label: String, what: &'static str, args: Vec<String>| {
            let weak = weak.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let job = GitJob::owned(what, args.clone());
                weak.update(cx, |this, cx| this.run(job, window, cx)).ok();
            })
        };
        // Item calling a method of the app.
        let act = |label: String, f: std::rc::Rc<dyn Fn(&mut Rameau, &mut Window, &mut Context<Rameau>)>| {
            let weak = weak.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let f = f.clone();
                weak.update(cx, |this, cx| f(this, window, cx)).ok();
            })
        };
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match this.menu_target.clone() {
            None => menu.label("Aucune action"),
            Some(MenuTarget::Commit(id)) => {
                let (full, short) = (id.to_string(), id.to_string()[..7].to_owned());
                let copy = full.clone();
                menu.item(git(format!("Checkout {short} (HEAD détachée)"), "Checkout", args(&["switch", "--detach", &full])))
                    .item(act("Nouvelle branche ici…".into(), std::rc::Rc::new(move |this, w, cx| this.new_branch_dialog(id, w, cx))))
                    .separator()
                    .item(git(format!("Merger {short} dans {head}"), "Merge", args(&["merge", "--no-edit", &full])))
                    .item(git(format!("Rebaser {head} sur {short}"), "Rebase", args(&["rebase", "--autostash", &full])))
                    .item(act("Rebase interactif à partir d'ici…".into(), std::rc::Rc::new(move |this, w, cx| this.interactive_rebase(id, w, cx))))
                    .separator()
                    .item(PopupMenuItem::new("Copier le hash").icon(IconName::Copy).on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
            }
            Some(MenuTarget::Branch(name, kind)) => {
                // `git switch feature` on a remote-only branch creates the local tracking branch.
                let local = match kind {
                    RefKind::Remote => name.split_once('/').map_or(name.as_str(), |(_, b)| b),
                    _ => name.as_str(),
                };
                let is_head = kind == RefKind::Head;
                let (n1, k1) = (name.clone(), kind);
                let delete_label = if kind == RefKind::Remote { "Supprimer sur le serveur…" } else { "Supprimer la branche…" };
                menu.item(git(format!("Checkout {name}"), "Checkout", args(&["switch", local])).disabled(is_head))
                    .separator()
                    .item(git(format!("Merger {name} dans {head}"), "Merge", args(&["merge", "--no-edit", &name])).disabled(is_head))
                    .item(git(format!("Rebaser {head} sur {name}"), "Rebase", args(&["rebase", "--autostash", &name])).disabled(is_head))
                    .separator()
                    .item(
                        act(delete_label.into(), std::rc::Rc::new(move |this, w, cx| this.delete_branch(n1.clone(), k1, w, cx)))
                            .disabled(is_head),
                    )
            }
            Some(MenuTarget::Stash(index)) => {
                let r = format!("stash@{{{index}}}");
                let drop_ref = r.clone();
                menu.item(git("Appliquer".into(), "Application du stash", args(&["stash", "apply", &r])))
                    .item(git("Appliquer et supprimer".into(), "Application du stash", args(&["stash", "pop", &r])))
                    .separator()
                    .item(act(
                        "Supprimer…".into(),
                        std::rc::Rc::new(move |this, w, cx| {
                            let job = GitJob::owned("Suppression du stash", vec!["stash".into(), "drop".into(), drop_ref.clone()]);
                            let msg = "Les modifications mises de côté dans ce stash seront perdues.".to_owned();
                            this.confirm(format!("Supprimer {drop_ref} ?"), msg, "Supprimer", job, w, cx)
                        }),
                    ))
            }
            Some(MenuTarget::File(path, untracked)) => {
                let p = path.clone();
                menu.item(act(
                    "Abandonner les modifications…".into(),
                    std::rc::Rc::new(move |this, w, cx| this.discard_file(p.clone(), untracked, w, cx)),
                ))
                .item(PopupMenuItem::new("Copier le chemin").icon(IconName::Copy).on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(path.clone()))))
            }
        }
    }
}

fn sidebar_row(id: (&'static str, usize), selected: bool) -> gpui::Stateful<gpui::Div> {
    let p = pal();
    div()
        .id(id)
        .mx_2()
        .pl(px(30.))
        .pr_2()
        .h(px(30.))
        .flex()
        .items_center()
        .gap_2p5()
        .rounded(px(7.))
        .cursor_pointer()
        .when(selected, |d| d.bg(c(p.selected)))
        .when(!selected, |d| d.hover(|s| s.bg(c(p.hover))))
}

fn ref_pill(r: &RefLabel, lane: gpui::Rgba) -> impl IntoElement {
    let p = pal();
    let pill = div().flex_none().flex().items_center().gap_1().h(px(20.)).px_2().rounded_full().text_xs();
    match r.kind {
        RefKind::Head => pill.bg(lane).text_color(c(p.bg)).font_weight(gpui::FontWeight::SEMIBOLD),
        RefKind::Local => pill.bg(gpui::Rgba { a: 0.16, ..lane }).text_color(lane).font_weight(gpui::FontWeight::MEDIUM),
        RefKind::Remote => pill
            .border_1()
            .border_color(gpui::Rgba { a: 0.4, ..lane })
            .text_color(lane)
            .child(icon("cloud").size(px(11.))),
        RefKind::Tag => pill.bg(c(p.border)).text_color(c(p.muted)),
    }
    .child(SharedString::from(r.name.clone()))
}

fn paint_graph(bounds: Bounds<Pixels>, row: &Row, is_merge: bool, window: &mut Window) {
    let o = bounds.origin;
    let x = |lane: usize| o.x + px(10. + LANE_W * lane as f32 + LANE_W / 2.);
    let (top, mid, bottom) = (o.y, o.y + px(ROW_H / 2.), o.y + px(ROW_H));
    let visible = |lane: usize| lane < MAX_GRAPH_LANES;
    for s in &row.segments {
        if !visible(s.from) || !visible(s.to) {
            continue;
        }
        let (a, b): (Point<Pixels>, Point<Pixels>) = match s.half {
            Half::Top => (point(x(s.from), top), point(x(s.to), mid)),
            Half::Bottom => (point(x(s.from), mid), point(x(s.to), bottom)),
        };
        let mut path = PathBuilder::stroke(px(2.));
        path.move_to(a);
        if a.x == b.x {
            path.line_to(b);
        } else {
            // Smooth S-curve between lanes.
            let my = (a.y + b.y) / 2.;
            path.cubic_bezier_to(b, point(a.x, my), point(b.x, my));
        }
        if let Ok(p) = path.build() {
            window.paint_path(p, lane_color(s.color));
        }
    }
    if visible(row.lane) {
        let r = px(5.);
        let color: Hsla = lane_color(row.color).into();
        let node = Bounds::new(point(x(row.lane) - r, mid - r), size(r * 2., r * 2.));
        // Ring in the background color so the node stands out from lines; merges are hollow.
        let fill: Hsla = if is_merge { c(pal().bg).into() } else { color };
        window.paint_quad(quad(node, r, fill, px(if is_merge { 2. } else { 0. }), color, Default::default()));
    }
}

impl Render for Rameau {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        self.viewport_w = f32::from(window.viewport_size().width);
        let right_w = if self.selection == Selection::None { 0. } else { self.panel_w() };
        self.center_w = self.viewport_w - self.sidebar_w() - right_w;
        let body: AnyElement = match (&self.proj, self.page) {
            (_, Page::Import) | (None, _) if !self.loading => self.import_page(cx).into_any_element(),
            (None, _) => div().flex_1().flex().items_center().justify_center().text_sm().text_color(c(p.muted)).child("Chargement…").into_any_element(),
            (Some(pr), _) => {
                let main = match &self.diff {
                    Some(d) => self.diff_view(d, cx).into_any_element(),
                    None => self.commit_list(pr, cx).into_any_element(),
                };
                let right = match self.selection {
                    Selection::Changes => Some(self.changes_panel(pr, cx).into_any_element()),
                    Selection::Commit(_) => self.details_panel(cx).map(|p| p.into_any_element()),
                    Selection::None => None,
                };
                let has_right = right.is_some();
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.sidebar(pr, cx))
                    .child(self.resize_handle(Side::Left, cx))
                    .child(div().id("center").flex_1().min_w_0().flex().flex_col().children(self.state_banner(pr, cx)).child(main))
                    .when(has_right, |d| d.child(self.resize_handle(Side::Right, cx)))
                    .children(right)
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(p.bg))
            .text_color(c(p.text))
            .key_context("Rameau")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &CloseDiff, _, cx| {
                this.close_diff();
                cx.notify();
            }))
            // Drop a folder anywhere to import it.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.paths().first() {
                    this.import_path(path.clone(), window, cx);
                }
            }))
            // Dragging a panel edge: follow the mouse anywhere in the window.
            .when(self.resizing.is_some(), |d| d.cursor_col_resize())
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                let Some(side) = this.resizing else { return };
                if e.pressed_button != Some(MouseButton::Left) {
                    // Released outside the window.
                    return this.end_resize(cx);
                }
                let width = f32::from(window.viewport_size().width);
                this.resize(side, f32::from(e.position.x), width, cx);
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_resize(cx)))
            .child(self.title_bar(cx))
            // Fade when switching page or project.
            .child(appear(
                div().flex_1().min_h_0().flex().child(body),
                ("page", key((self.page == Page::Import, self.dir()))),
                0.,
                0.,
            ))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
