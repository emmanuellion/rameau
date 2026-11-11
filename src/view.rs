//! Main window layout: title bar, sidebar, history, changes and details panels, context menus.

use git2::RepositoryState;
use gpui::{
    Animation, AnimationExt, MouseMoveEvent, ease_out_quint, percentage, radians,
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, Corner, ExternalPaths, img, Hsla, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, PathBuilder, Pixels, Point, Render, SharedString, Styled, WeakEntity,
    Window, canvas, div, point, prelude::*, px, quad, relative, size, uniform_list,
};
use gpui_component::{
    Disableable, Icon, IconName, Root, Sizable, TitleBar,
    button::ButtonVariants,
    input::Input,
    menu::{ContextMenu, ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
};

use crate::app::{CloseDiff, GitJob, Login, MenuTarget, Page, Project, Rameau, SelectNext, SelectPrev, Selection, Side, View};
use crate::graph::{Half, Row};
use crate::repo::{Check, DiffSource, FileEntry, RefKind, RefLabel};
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
/// Height of every line of the file lists (headers included): they are virtualized.
const LIST_ROW_H: f32 = 34.;

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

/// An operation git left half-done, and the commands of the banner's buttons.
struct Operation {
    title: &'static str,
    /// None: nothing to continue from the app (bisect is driven from a terminal).
    cont: Option<&'static [&'static str]>,
    abort: &'static [&'static str],
    abort_label: &'static str,
}

fn operation(state: RepositoryState) -> Option<Operation> {
    use RepositoryState::*;
    let op = |title, cont, abort| Some(Operation { title, cont: Some(cont), abort, abort_label: "Annuler l'opération" });
    match state {
        Clean => None,
        Merge => op("Merge en cours", &["commit", "--no-edit"], &["merge", "--abort"]),
        Rebase | RebaseInteractive | RebaseMerge => op("Rebase en cours", &["rebase", "--continue"], &["rebase", "--abort"]),
        CherryPick | CherryPickSequence => op("Cherry-pick en cours", &["cherry-pick", "--continue"], &["cherry-pick", "--abort"]),
        Revert | RevertSequence => op("Revert en cours", &["revert", "--continue"], &["revert", "--abort"]),
        ApplyMailbox | ApplyMailboxOrRebase => op("Application de patchs en cours", &["am", "--continue"], &["am", "--abort"]),
        Bisect => Some(Operation { title: "Bisect en cours", cont: None, abort: &["bisect", "reset"], abort_label: "Arrêter le bisect" }),
    }
}

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
            return TitleBar::new().h(px(TITLE_BAR_H)).child(bar.child(div().flex_1()).child(self.account_button(cx)));
        };
        let h = &pr.history;
        let busy = self.busy.is_some();
        let (ahead, behind) = h.ahead_behind.unwrap_or((0, 0));
        let push_primary = h.has_commits && (ahead > 0 || h.ahead_behind.is_none());
        let weak = cx.weak_entity();
        let branch = button("branch")
            .ghost()
            .icon(icon("git-branch"))
            .label(h.head.clone())
            .child(Icon::new(IconName::ChevronDown).xsmall().text_color(c(p.faint)))
            .tooltip("Changer de branche")
            .dropdown_menu(move |menu, _, cx| Self::branches_menu(&weak, menu, cx));
        TitleBar::new().h(px(TITLE_BAR_H)).child(
            bar.child(branch)
            .children(self.pull_request_button(pr))
            .when(h.has_commits && h.head_branch.is_some() && h.ahead_behind.is_none(), |d| {
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
                        this.run(GitJob::new("Fetch", &["fetch", "--all", "--prune"]).network(), window, cx)
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
                    .disabled(busy || h.head_branch.is_none() || !h.has_commits)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.push(window, cx))),
            )
            .child(div().w(px(6.)))
            .child(self.account_button(cx)),
        )
    }

    /// Author's GitHub picture when known, else their initials.
    fn author_avatar(&self, email: &str, name: &str, size: f32) -> AnyElement {
        match self.avatar_of(email) {
            Some(image) => img(image).size(px(size)).flex_none().rounded_full().into_any_element(),
            None => avatar(name, size).into_any_element(),
        }
    }

    /// CI result of a branch on GitHub: ✓, ✗ or a dot while it runs.
    fn ci_icon(&self, branch: &str) -> Option<AnyElement> {
        let p = pal();
        let checks = *self.gh_info.as_ref()?.checks.get(branch)?;
        Some(match checks {
            crate::github::Checks::Success => Icon::new(IconName::Check).xsmall().text_color(c(p.green)).into_any_element(),
            crate::github::Checks::Failure => Icon::new(IconName::Close).xsmall().text_color(c(p.red)).into_any_element(),
            crate::github::Checks::Pending => div().size(px(7.)).mx(px(2.5)).rounded_full().bg(c(p.yellow)).into_any_element(),
        })
    }

    /// Next to the current branch: its pull request, or a button to open one once the branch is pushed.
    fn pull_request_button(&self, pr: &Project) -> Option<AnyElement> {
        let info = self.gh_info.as_ref()?;
        let branch = pr.history.head_branch.clone()?;
        if let Some(pull) = self.pull_request(&branch) {
            let url = pull.url.clone();
            let label = if pull.draft { format!("PR #{} (brouillon)", pull.number) } else { format!("PR #{}", pull.number) };
            return Some(
                button_sm("open-pr")
                    .ghost()
                    .icon(IconName::GitHub)
                    .label(label)
                    .children(self.ci_icon(&branch))
                    .tooltip(pull.title.clone())
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element(),
            );
        }
        if pr.history.ahead_behind.is_none() || branch == info.default_branch {
            return self.ci_icon(&branch);
        }
        let url = format!("https://github.com/{}/compare/{branch}?expand=1", info.slug);
        Some(
            div()
                .flex()
                .items_center()
                .gap_1()
                .children(self.ci_icon(&branch))
                .child(
                    button_sm("create-pr")
                        .ghost()
                        .icon(IconName::GitHub)
                        .label("Créer une pull request")
                        .tooltip("Ouvre la page GitHub de création, pré-remplie avec cette branche")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
                .into_any_element(),
        )
    }

    /// GitHub account at the right of the title bar: its picture and a menu (identity, theme,
    /// sign-out); a sign-in button when signed out, the code to type while signing in.
    fn account_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = pal();
        let weak = cx.weak_entity();
        let menu = move |menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>| Self::account_menu(&weak, menu, cx);
        match &self.login {
            Login::Waiting(code) => div()
                .flex()
                .items_center()
                .gap_2()
                .pl_3()
                .pr_1()
                .h(px(30.))
                .rounded(px(8.))
                .bg(alpha(p.accent, 0.12))
                .child(div().text_xs().text_color(c(p.muted)).child("Code GitHub (copié) :"))
                .child(div().text_sm().font_family(mono()).font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(p.text)).child(code.clone()))
                .child(button_sm("cancel-login").ghost().label("Annuler").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.logout(cx))))
                .into_any_element(),
            Login::Out => button("account")
                .ghost()
                .icon(IconName::GitHub)
                .label("Se connecter")
                .dropdown_menu_with_anchor(Corner::TopRight, menu)
                .into_any_element(),
            Login::In(a) => {
                let pic = match &self.avatar {
                    Some(image) => img(image.clone()).size(px(26.)).rounded_full().into_any_element(),
                    None => avatar(&a.login, 26.).into_any_element(),
                };
                button("account")
                    .ghost()
                    .px_1p5()
                    .child(pic)
                    .child(Icon::new(IconName::ChevronDown).xsmall().text_color(c(p.faint)))
                    .dropdown_menu_with_anchor(Corner::TopRight, menu)
                    .into_any_element()
            }
        }
    }

    fn account_menu(weak: &WeakEntity<Self>, mut menu: PopupMenu, cx: &mut App) -> PopupMenu {
        let Some(this) = weak.upgrade() else { return menu };
        let this = this.read(cx);
        let act = |label: &str, f: fn(&mut Rameau, &mut Window, &mut Context<Rameau>)| {
            let weak = weak.clone();
            PopupMenuItem::new(label.to_owned()).on_click(move |_, window, cx| {
                weak.update(cx, |this, cx| f(this, window, cx)).ok();
            })
        };
        if let Login::In(a) = &this.login {
            let who = match &a.name {
                Some(name) => format!("{name} ({})", a.login),
                None => a.login.clone(),
            };
            menu = menu.label(who).separator();
        }
        let theme = if this.config.light { "Thème sombre" } else { "Thème clair" };
        menu = menu
            .item(act("Identité git (nom, e-mail)…", |this, window, cx| this.identity_dialog(false, window, cx)).icon(IconName::User))
            .item(act(theme, |this, window, cx| this.toggle_theme(window, cx)).icon(if this.config.light { IconName::Moon } else { IconName::Sun }))
            .separator();
        match this.login {
            Login::In(_) => menu.item(act("Se déconnecter de GitHub", |this, _, cx| this.logout(cx))),
            _ => menu.item(act("Se connecter avec GitHub…", |this, window, cx| this.login(window, cx)).icon(IconName::GitHub)),
        }
    }

    /// Local branches to switch to, and a new branch from the current commit.
    fn branches_menu(weak: &WeakEntity<Self>, mut menu: PopupMenu, cx: &mut App) -> PopupMenu {
        let Some(this) = weak.upgrade() else { return menu };
        let this = this.read(cx);
        let Some(pr) = &this.proj else { return menu };
        for b in pr.history.branches.iter().filter(|b| b.kind != RefKind::Remote) {
            let (weak, name) = (weak.clone(), b.name.clone());
            menu = menu.item(PopupMenuItem::new(b.name.clone()).checked(b.kind == RefKind::Head).on_click(move |_, window, cx| {
                let job = GitJob::owned("Checkout", vec!["switch".into(), name.clone()]);
                weak.update(cx, |this, cx| this.run(job, window, cx)).ok();
            }));
        }
        let head = pr.repo.head().ok().and_then(|h| h.target());
        let weak = weak.clone();
        menu.separator().item(PopupMenuItem::new("Nouvelle branche…").icon(IconName::Plus).disabled(head.is_none()).on_click(move |_, window, cx| {
            if let Some(at) = head {
                weak.update(cx, |this, cx| this.new_branch_dialog(at, window, cx)).ok();
            }
        }))
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
        let line = div().w_full().h(px(SIDE_ROW_H)).flex().flex_col().justify_end();
        match item {
            SideItem::Header(g, count) => {
                let group = &GROUPS[g];
                line.child(self.section_header(group.key, group.title, icon(group.icon), count, cx)).into_any_element()
            }
            SideItem::Branch(i) => {
                let b = &h.branches[i];
                // Rows not read yet have no lane: their branch takes its name's color meanwhile.
                let color = match b.row.and_then(|r| h.rows.get(r)) {
                    Some(r) => lane_color(r.color),
                    None => lane_color(crate::graph::color_index(b.name.rsplit('/').next().unwrap_or(&b.name).as_bytes())),
                };
                let row = b.row;
                let is_head = b.kind == RefKind::Head;
                let target = MenuTarget::Branch(b.name.clone(), b.kind);
                let selected = row.is_some_and(|r| Some(h.order[r]) == selected_id);
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
                        .when(b.kind != RefKind::Remote, |d| {
                            d.children(self.ci_icon(&b.name)).children(self.pull_request(&b.name).map(|pr| pr_pill(("pr", i), pr)))
                        })
                        .when(is_head, |d| d.child(Icon::new(IconName::Check).xsmall().text_color(c(p.accent))))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_target = Some(target.clone())))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(r) = row {
                                this.reveal_row(r, cx);
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.view = View::History;
                            this.select_commit(id, cx)
                        })),
                )
                .into_any_element()
            }
        }
    }

    /// Entry at the top of the sidebar switching the main area.
    fn nav_item(&self, view: View, ico: Icon, title: &'static str, count: Option<usize>, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let active = self.view == view;
        div()
            .id(title)
            .mx_2()
            .px_2p5()
            .h(px(34.))
            .flex()
            .items_center()
            .gap_2p5()
            .rounded(px(8.))
            .cursor_pointer()
            .when(!active, |d| d.hover(|s| s.bg(c(p.hover))))
            .child(ico.small().text_color(c(if active { p.accent } else { p.muted })))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(c(if active { p.text } else { p.muted }))
                    .when(active, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
                    .child(title),
            )
            .children(count.filter(|n| *n > 0).map(|n| badge(n, false)))
            .on_click(cx.listener(move |this, _, _, cx| this.set_view(view, cx)))
            // The active item's background fades in when it becomes active.
            .with_animation((title, active as usize), Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()), move |d, t| {
                if active { d.bg(alpha(p.selected, t)) } else { d }
            })
    }

    fn sidebar(&self, pr: &Project, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
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
        let changed = pr.history.changes.files.len();
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
                    .flex_none()
                    .pt_3()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .border_b_1()
                    .border_color(c(p.border))
                    .child(self.nav_item(View::Changes, icon("square-pen"), "Modifications", Some(changed), cx))
                    .child(self.nav_item(View::History, icon("history"), "Historique", None, cx)),
            )
            .child(self.with_menu(
                div()
                    .id("branches")
                    .flex_1()
                    .min_h_0()
                    .pt_1()
                    .flex()
                    .flex_col()
                    .child(list),
                cx,
            ))
    }

    /// Banner shown while a merge/rebase/cherry-pick waits for the user.
    fn state_banner(&self, pr: &Project, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = pal();
        let op = operation(pr.history.state)?;
        let conflicts = pr.history.changes.conflicted.len();
        let hint = match (op.cont, conflicts) {
            (None, _) => "Recherche du commit fautif lancée dans un terminal : continue-la là-bas, ou arrête-la ici.".to_owned(),
            (Some(_), 0) => "Conflits résolus : tu peux continuer.".to_owned(),
            (Some(_), 1) => "1 conflit à résoudre : corrige le fichier puis coche-le.".to_owned(),
            (Some(_), n) => format!("{n} conflits à résoudre : corrige les fichiers puis coche-les."),
        };
        let words = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (title, cont, abort, bisect) = (op.title, op.cont.map(words), words(op.abort), op.cont.is_none());
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
                .child(button_sm("abort-op").ghost().label(op.abort_label).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        let msg = match bisect {
                            true => "Le bisect s'arrête et tu reviens sur la branche de départ.",
                            false => "Le dépôt revient à son état d'avant l'opération : les conflits déjà résolus seront perdus.",
                        };
                        let job = GitJob::owned("Annulation", abort.clone());
                        this.confirm(format!("{} ?", op.abort_label), msg.to_owned(), op.abort_label, job, window, cx)
                    },
                )))
                .children(cont.map(|cont| {
                    button_sm("continue-op")
                        .primary()
                        .label("Continuer")
                        .disabled(conflicts > 0)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.run(GitJob::owned("Continuer", cont.clone()), window, cx)))
                })),
        )
    }

    fn graph_width(pr: &Project) -> f32 {
        pr.history.max_lanes.min(crate::graph::MAX_LANES) as f32 * LANE_W + 10.
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
                .border_color(gpui::transparent_black())
                .hover(|s| s.bg(c(p.hover)))
                .on_click(cx.listener(|this, _, _, cx| this.set_view(View::Changes, cx)))
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
        let is_merge = row.segments.iter().filter(|s| s.half == Half::Bottom && s.from as usize == row.lane).count() > 1;
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
                    .child(self.author_avatar(&cm.email, &cm.author, 20.))
                    .when(cols.author_name, |d| d.child(div().min_w_0().truncate().text_xs().text_color(c(p.muted)).child(cm.author.clone()))),
            )
            .when(cols.date, |d| {
                d.child(div().w(px(DATE_W)).flex_none().truncate().text_xs().text_color(c(p.muted)).child(crate::repo::relative_time(cm.time)))
            })
            .when(cols.hash, |d| {
                d.child(div().w(px(HASH_W)).flex_none().text_xs().font_family(mono()).text_color(c(p.faint)).child(cm.id.to_string()[..7].to_owned()))
            })
    }

    /// A commit found by the history search.
    fn found_row(&self, i: usize, f: &crate::repo::Found, cx: &mut Context<Self>) -> AnyElement {
        let p = pal();
        let cols = Columns::for_width(self.center_w);
        let id = f.id;
        let selected = self.selection == Selection::Commit(id);
        div()
            .id(("found", i))
            .h(px(ROW_H))
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .pl_4()
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
            .child(div().size(px(8.)).flex_none().rounded_full().bg(c(p.accent)))
            .child(div().flex_1().min_w_0().truncate().text_sm().text_color(c(p.text)).child(f.summary.clone()))
            .child(
                div()
                    .w(px(cols.author_w()))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.author_avatar(&f.email, &f.author, 20.))
                    .when(cols.author_name, |d| d.child(div().min_w_0().truncate().text_xs().text_color(c(p.muted)).child(f.author.clone()))),
            )
            .when(cols.date, |d| d.child(div().w(px(DATE_W)).flex_none().truncate().text_xs().text_color(c(p.muted)).child(crate::repo::relative_time(f.time))))
            .when(cols.hash, |d| d.child(div().w(px(HASH_W)).flex_none().text_xs().font_family(mono()).text_color(c(p.faint)).child(id.to_string()[..7].to_owned())))
            .into_any_element()
    }

    fn commit_list(&self, pr: &Project, cx: &mut Context<Self>) -> AnyElement {
        let p = pal();
        let cols = Columns::for_width(self.center_w);
        let col = |text: &'static str| div().text_xs().font_weight(gpui::FontWeight::MEDIUM).text_color(c(p.faint)).child(text);
        let search = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(c(p.border))
            .child(div().flex_1().max_w(px(520.)).child(if self.search_open || self.search_hits.is_some() {
                Input::new(&self.history_search)
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small().text_color(c(p.faint)))
                    .into_any_element()
            } else {
                // Looks like the field, opens it on click.
                div()
                    .id("search-closed")
                    .h(px(32.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(c(p.border))
                    .cursor_text()
                    .text_sm()
                    .text_color(c(p.faint))
                    .hover(|s| s.border_color(c(p.muted)))
                    .child(Icon::new(IconName::Search).small().text_color(c(p.faint)))
                    .child("Rechercher un commit : message, auteur ou hash")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_search(window, cx)))
                    .into_any_element()
            }))
            .children(self.search_hits.as_ref().map(|(_, hits)| {
                let n = hits.len();
                div().text_xs().text_color(c(p.muted)).child(match n {
                    0 => "Aucun commit trouvé".to_owned(),
                    1 => "1 commit trouvé".to_owned(),
                    n => format!("{n} commits trouvés"),
                })
            }));
        // Searching: the commits found, newest first, without the graph (their lanes would be cut).
        if let Some((_, hits)) = &self.search_hits {
            let list = uniform_list(
                "found",
                hits.len(),
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    let Some((index, hits)) = &this.search_hits else { return vec![] };
                    range.filter_map(|i| hits.get(i).map(|&h| this.found_row(i, &index[h], cx))).collect::<Vec<_>>()
                }),
            )
            .flex_1();
            let found = self.with_menu(div().flex_1().min_h_0().flex().flex_col().child(list), cx);
            return div().flex_1().min_h_0().flex().flex_col().child(search).child(found).into_any_element();
        }
        let graph = self.with_menu(
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
                    cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                        let Some(pr) = &this.proj else { return vec![] };
                        // Near the last commit read: read the next page, shown at the next frame.
                        if pr.history.has_more() && range.end + 30 >= pr.history.commits.len() {
                            let upto = pr.history.commits.len() + crate::repo::PAGE;
                            cx.defer_in(window, move |this, _, cx| {
                                this.load_commits(upto);
                                this.want_avatars(cx);
                                cx.notify();
                            });
                        }
                        range.map(|ix| this.commit_row(pr, ix, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1(),
            ),
            cx,
        )
        ;
        div().flex_1().min_h_0().flex().flex_col().child(search).child(graph).into_any_element()
    }

    /// File of a commit, in the details panel.
    fn file_row(&self, id: (&'static str, usize), (status, path): &(char, String), source: DiffSource, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let open = self.diff.as_ref().is_some_and(|d| d.path == *path && d.source == source);
        let pth = path.clone();
        file_line(id, None, *status, path, open)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_diff(source.clone(), pth.clone(), false, cx)))
    }

    /// Changed file of the working copy: its box says how much of it goes into the commit.
    fn change_row(&self, ix: usize, f: &FileEntry, cx: &mut Context<Self>) -> AnyElement {
        let open = self.diff.as_ref().is_some_and(|d| d.path == f.path && !matches!(d.source, DiffSource::Commit(_)));
        // Checked whole: take it out of the commit. Otherwise (or a conflict marked solved): put it all in.
        let args: Vec<String> = match f.check {
            Check::All => vec!["reset".into(), "-q".into(), "--".into(), crate::git::literal(&f.path)],
            _ => vec!["add".into(), "--".into(), crate::git::literal(&f.path)],
        };
        let what = if f.check == Check::All { "Retrait" } else { "Ajout" };
        let (source, path) = (f.diff_source(), f.path.clone());
        let target = MenuTarget::File(f.path.clone(), f.status == 'A' && f.check == Check::None);
        div()
            .w_full()
            .h(px(LIST_ROW_H))
            .flex()
            .flex_col()
            .justify_center()
            .child(
                file_line(
                    ("change", ix),
                    Some(check_box(("check", ix), f.check).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.run(GitJob::owned(what, args.clone()).status_only(), window, cx);
                    }))),
                    f.status,
                    &f.path,
                    open,
                )
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_target = Some(target.clone())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_diff(source.clone(), path.clone(), false, cx))),
            )
            .into_any_element()
    }

    /// Above the message: amend the last commit (not pushed yet), or undo it.
    fn last_commit_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let check = if self.amend { Check::All } else { Check::None };
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id("amend")
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(check_box("amend-box", check))
                    .child(div().text_xs().text_color(c(p.muted)).child("Modifier le dernier commit"))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_amend(window, cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("undo-commit")
                    .text_xs()
                    .text_color(c(p.faint))
                    .cursor_pointer()
                    .hover(|s| s.text_color(c(p.accent)))
                    .child("Annuler le dernier commit")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.undo_commit(window, cx))),
            )
    }

    /// Changes view: the changed files and the commit message on the left, the diff on the right.
    fn changes_view(&self, pr: &Project, cx: &mut Context<Self>) -> AnyElement {
        let p = pal();
        let ch = &pr.history.changes;
        if ch.files.is_empty() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_8()
                .child(appear(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_3()
                        .max_w(px(420.))
                        .child(Icon::new(IconName::CircleCheck).size(px(40.)).text_color(c(p.green)))
                        .child(div().text_xl().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(p.text)).child("Aucune modification locale"))
                        .child(div().text_sm().text_center().text_color(c(p.muted)).child(
                            "Tes fichiers sont identiques au dernier commit. Modifie-les dans ton éditeur : \
                             ils apparaîtront ici, prêts à être cochés pour le prochain commit.",
                        ))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    button("show-history")
                                        .outline()
                                        .icon(icon("history"))
                                        .label("Voir l'historique")
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_view(View::History, cx))),
                                )
                                .when(crate::app::rewritable(&pr.history), |d| {
                                    d.child(
                                        button("undo-commit-empty")
                                            .ghost()
                                            .icon(IconName::Undo2)
                                            .label("Annuler le dernier commit")
                                            .tooltip("Ses modifications reviennent ici, cochées, avec son message")
                                            .disabled(self.busy.is_some())
                                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.undo_commit(window, cx))),
                                    )
                                }),
                        ),
                    "no-changes",
                    8.,
                    0.,
                ))
                .into_any_element();
        }
        // Virtualized: a project without .gitignore can list tens of thousands of files.
        let files = uniform_list(
            "changes-list",
            ch.files.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let Some(pr) = &this.proj else { return vec![] };
                let files = &pr.history.changes.files;
                range.filter_map(|ix| files.get(ix).map(|f| this.change_row(ix, f, cx))).collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .px_2();
        let all = if ch.files.iter().all(|f| f.check == Check::All) {
            Check::All
        } else if ch.files.iter().any(|f| f.check != Check::None) {
            Check::Part
        } else {
            Check::None
        };
        let n = ch.files.len();
        let checked = ch.staged.len();
        let branch = pr.history.head.clone();
        let key = if cfg!(target_os = "macos") { "⌘↩" } else { "Ctrl+↩" };
        let rewritable = crate::app::rewritable(&pr.history);
        let amend = self.amend && rewritable;
        let column = div()
            .id("changes-column")
            .w(px(self.files_w()))
            .flex_none()
            .flex()
            .flex_col()
            .bg(c(p.panel))
            .border_r_1()
            .border_color(c(p.border))
            .child(
                div()
                    .h(px(40.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .px_4()
                    .border_b_1()
                    .border_color(c(p.border))
                    .child(check_box("check-all", all).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let (what, args): (&'static str, &[&str]) = match all {
                            Check::All => ("Tout décocher", &["reset", "-q"]),
                            _ => ("Tout cocher", &["add", "-A"]),
                        };
                        this.run(GitJob::new(what, args).status_only(), window, cx)
                    })))
                    .child(div().flex_1().text_sm().font_weight(gpui::FontWeight::MEDIUM).text_color(c(p.text)).child(match n {
                        1 => "1 fichier modifié".to_owned(),
                        n => format!("{n} fichiers modifiés"),
                    }))
                    .child(div().text_xs().text_color(c(p.faint)).child(format!("{checked} coché{}", if checked > 1 { "s" } else { "" }))),
            )
            .child(self.with_menu(div().id("changes-files").flex_1().min_h_0().flex().flex_col().pt_1().child(files), cx))
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .border_t_1()
                    .border_color(c(p.border))
                    .when(rewritable, |d| d.child(self.last_commit_bar(cx)))
                    .child(Input::new(&self.summary))
                    .child(Input::new(&self.description).h(px(84.)))
                    .child(
                        button("commit")
                            .primary()
                            .w_full()
                            .h(px(38.))
                            .icon(icon("git-commit"))
                            .label(match (amend, checked) {
                                (true, _) => "Modifier le dernier commit".to_owned(),
                                (_, 0) => format!("Commit sur {branch}"),
                                (_, 1) => format!("Commit 1 fichier sur {branch}"),
                                (_, n) => format!("Commit {n} fichiers sur {branch}"),
                            })
                            .disabled((checked == 0 && !amend) || self.busy.is_some())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.commit(window, cx))),
                    )
                    .child(div().text_xs().text_color(c(p.faint)).child(match (amend, checked) {
                        (true, _) => "Les fichiers cochés rejoignent le dernier commit".to_owned(),
                        (_, 0) => "Coche les fichiers à inclure dans le commit".to_owned(),
                        _ => format!("{key} pour valider · clic droit : abandonner un fichier"),
                    })),
            );
        let diff = match &self.diff {
            Some(d) => self.diff_view(d, cx).into_any_element(),
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(c(p.faint))
                .child("Choisis un fichier pour voir ses modifications")
                .into_any_element(),
        };
        div().flex_1().min_w_0().flex().child(column).child(div().flex_1().min_w_0().flex().flex_col().child(diff)).into_any_element()
    }

    fn details_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let p = pal();
        let d = self.details.as_ref()?;
        let Selection::Commit(id) = self.selection else { return None };
        let (title, body) = d.message.split_once('\n').unwrap_or((&d.message, ""));
        // Virtualized: a commit can touch tens of thousands of files (imports, vendoring).
        let files = uniform_list(
            "details-files",
            d.files.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                let Some(d) = &this.details else { return vec![] };
                range
                    .filter_map(|i| d.files.get(i).map(|f| div().w_full().h(px(LIST_ROW_H)).flex().flex_col().justify_center().child(this.file_row(("file", i), f, DiffSource::Commit(id), cx))))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1();
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
                .flex()
                .flex_col()
                .child(appear(
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    // Message, author and hash scroll on their own: a long message never hides the files.
                    div().id("details-info").flex_none().max_h(relative(0.5)).overflow_y_scroll().p_5().pb_2().flex().flex_col().gap_5().child(
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
                        .child(self.author_avatar(&d.email, &d.author, 34.))
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
                )
                .child(div().flex_none().flex().items_center().gap_2().px_5().pt_3().pb_1().child(label("FICHIERS")).child(badge(d.files.len(), false)))
                .child(div().flex_1().min_h_0().flex().flex_col().px_3().pb_3().child(files)),
                // Replays for each commit selected.
                ("details-in", key(id)),
                6.,
                0.,
            )),
        )
    }

    /// Invisible 6px grip on a panel edge: a thin accent line shows on hover and while dragging.
    /// Double-click restores the default width.
    /// `x`: the panel edge, from the left of the window. Drawn over the panels as the last child of
    /// their row, not with `deferred`: dialogs and menus, drawn after the row, must stay above it.
    fn resize_handle(&self, side: Side, x: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let active = self.resizing == Some(side);
        let id = match side {
            Side::Left => "resize-left",
            Side::Right => "resize-right",
            Side::Files => "resize-files",
        };
        div()
                    .id(id)
                    .group(id)
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(x - 3.))
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
                    }))
    }

    /// Right-click menu on a zone. Its rows set `menu_target` on right mouse down; the zone
    /// clears it first, so a right click on empty space never shows the previous row's actions.
    fn with_menu<E>(&self, el: E, cx: &mut Context<Self>) -> ContextMenu<E>
    where
        E: ParentElement + Styled + InteractiveElement + IntoElement + 'static,
    {
        let weak = cx.weak_entity();
        el.capture_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, _, _| {
            if e.button == MouseButton::Right {
                this.menu_target = None;
            }
        }))
        .context_menu(move |menu, _, cx| Self::build_menu(&weak, menu, cx))
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
                let locals: Vec<&str> = pr.history.branches.iter().filter(|b| b.kind != RefKind::Remote).map(|b| b.name.as_str()).collect();
                let checkout = crate::app::checkout_args(&name, kind == RefKind::Remote, &locals);
                let is_head = kind == RefKind::Head;
                let (n1, k1) = (name.clone(), kind);
                let delete_label = if kind == RefKind::Remote { "Supprimer sur le serveur…" } else { "Supprimer la branche…" };
                menu.item(git(format!("Checkout {name}"), "Checkout", checkout).disabled(is_head))
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

/// A file line: its box when it can be checked, status letter, name, then its folder, faded.
fn file_line(id: impl Into<gpui::ElementId>, check: Option<gpui::Stateful<gpui::Div>>, status: char, path: &str, open: bool) -> gpui::Stateful<gpui::Div> {
    let p = pal();
    let (dir, name) = path.rsplit_once('/').map_or(("", path), |(d, n)| (d, n));
    div()
        .id(id.into())
        .h(px(30.))
        .flex()
        .items_center()
        .gap_2p5()
        .px_2()
        .rounded(px(7.))
        .cursor_pointer()
        .when(open, |d| d.bg(c(p.selected)))
        .when(!open, |d| d.hover(|s| s.bg(c(p.hover))))
        .children(check)
        .child(
            div()
                .size(px(18.))
                .flex_none()
                .rounded(px(4.))
                .bg(gpui::Rgba { a: 0.14, ..status_color(status) })
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(status_color(status))
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
}

/// Box of a changed file (or of all of them): checked, half (some lines only), or empty.
fn check_box(id: impl Into<gpui::ElementId>, check: Check) -> gpui::Stateful<gpui::Div> {
    let p = pal();
    let filled = check != Check::None;
    div()
        .id(id.into())
        .size(px(16.))
        .flex_none()
        .rounded(px(5.))
        .border_1()
        .border_color(c(if filled { p.accent } else { p.faint }))
        .when(filled, |d| d.bg(c(p.accent)))
        .hover(|s| s.border_color(c(p.accent)))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .when(filled, |d| {
            let mark = if check == Check::All { IconName::Check } else { IconName::Minus };
            d.child(Icon::new(mark).text_color(c(p.bg)).with_animation(
                ("pop", check as usize),
                Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                |icon, t| icon.size(px(6. + 6. * t)),
            ))
        })
}

/// "#123" next to a branch with an open pull request; opens it on GitHub.
fn pr_pill(id: (&'static str, usize), pr: &crate::github::PullRequest) -> impl IntoElement {
    let p = pal();
    let url = pr.url.clone();
    div()
        .id(id)
        .flex_none()
        .px_1p5()
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .bg(alpha(p.green, 0.14))
        .text_xs()
        .text_color(c(p.green))
        .cursor_pointer()
        .hover(|s| s.bg(alpha(p.green, 0.26)))
        .child(format!("#{}", pr.number))
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            cx.open_url(&url)
        })
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
    let visible = |lane: usize| lane < crate::graph::MAX_LANES;
    for s in &row.segments {
        let (from, to) = (s.from as usize, s.to as usize);
        let (a, b): (Point<Pixels>, Point<Pixels>) = match s.half {
            Half::Top => (point(x(from), top), point(x(to), mid)),
            Half::Bottom => (point(x(from), mid), point(x(to), bottom)),
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
            window.paint_path(p, lane_color(s.color as usize));
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
        let right_w = if self.view == View::History && self.details.is_some() { self.panel_w() } else { 0. };
        self.center_w = self.viewport_w - self.sidebar_w() - right_w;
        let body: AnyElement = match (&self.proj, self.page) {
            (_, Page::Import) | (None, _) if !self.loading => self.import_page(cx).into_any_element(),
            (None, _) => div().flex_1().flex().items_center().justify_center().text_sm().text_color(c(p.muted)).child("Chargement…").into_any_element(),
            (Some(pr), _) => {
                let (main, right) = match (self.view, &self.diff) {
                    (View::Changes, _) => (self.changes_view(pr, cx), None),
                    (View::History, Some(d)) => (self.diff_view(d, cx).into_any_element(), self.details_panel(cx).map(|p| p.into_any_element())),
                    (View::History, None) => (self.commit_list(pr, cx), self.details_panel(cx).map(|p| p.into_any_element())),
                };
                let has_right = right.is_some();
                let changes = self.view == View::Changes && !pr.history.changes.files.is_empty();
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .relative()
                    .child(self.sidebar(pr, cx))
                    .child(
                        div()
                            .id("center")
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .children(self.state_banner(pr, cx).map(|b| appear(div().child(b), "banner", -8., 0.)))
                            .child(appear(div().flex_1().min_h_0().flex().flex_col().child(main), ("view", key(self.view)), 0., 0.)),
                    )
                    .children(right)
                    .child(self.resize_handle(Side::Left, self.sidebar_w(), cx))
                    .when(has_right, |d| d.child(self.resize_handle(Side::Right, self.viewport_w - self.panel_w(), cx)))
                    .when(changes, |d| d.child(self.resize_handle(Side::Files, self.sidebar_w() + self.files_w(), cx)))
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
            // Fade when switching page, project or theme.
            .child(appear(
                div().flex_1().min_h_0().flex().child(body),
                ("page", key((self.page == Page::Import, self.dir(), self.config.light))),
                0.,
                0.,
            ))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unfinished_operation_gets_its_own_commands() {
        let dir = crate::app::tests::temp("bisect");
        let git = |a: &[&str]| crate::app::tests::git(&dir, a);
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "c1"]);
        git(&["commit", "-q", "--allow-empty", "-m", "c2"]);
        git(&["bisect", "start", "HEAD", "HEAD~1"]);
        let state = git2::Repository::open(&dir).unwrap().state();
        let op = operation(state).unwrap();
        assert_eq!((op.title, op.cont, op.abort), ("Bisect en cours", None, &["bisect", "reset"][..]));
        // The banner's abort command really ends it.
        let a: Vec<&str> = op.abort.to_vec();
        git(&a);
        assert!(operation(git2::Repository::open(&dir).unwrap().state()).is_none());
        assert_eq!(operation(RepositoryState::Merge).unwrap().abort, ["merge", "--abort"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
