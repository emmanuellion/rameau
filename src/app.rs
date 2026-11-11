//! Application state and actions. Rendering lives in `view.rs`, `diff_view.rs` and `import.rs`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use git2::{Oid, Repository};
use gpui::{
    ClipboardItem, Context, Entity, FocusHandle, HighlightStyle, Image, PathPromptOptions, ScrollStrategy, SharedString,
    Subscription, Task, UniformListScrollHandle, Window, actions, div, prelude::*, px,
};
use gpui_component::{
    Sizable, WindowExt,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    notification::Notification,
};

use crate::config::{self, Config};
use crate::rebase::RebaseEditor;
use crate::repo::{self, Details, DiffLine, DiffSource, History, LineKind, RefKind};
use crate::{diff_view, git, github, patch, theme, ui};

actions!(rameau, [SelectPrev, SelectNext, CloseDiff]);

pub struct Project {
    pub path: PathBuf,
    pub repo: Repository,
    pub history: History,
}

#[derive(Clone, PartialEq)]
pub enum Selection {
    None,
    Commit(Oid),
}

/// What the main area shows, picked at the top of the sidebar.
#[derive(Clone, Copy, PartialEq, Hash)]
pub enum View {
    /// Changed files to check, the commit message, the diff of a file.
    Changes,
    /// The colored graph, the details of a commit.
    History,
}

#[derive(Clone)]
pub enum MenuTarget {
    Commit(Oid),
    Branch(String, RefKind),
    Stash(usize),
    /// Unstaged file (path, untracked).
    File(String, bool),
}

pub enum Login {
    Out,
    Waiting(String),
    In(github::Account),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Page {
    Repo,
    Import,
}

pub struct OpenDiff {
    pub source: DiffSource,
    pub path: String,
    pub lines: Vec<DiffLine>,
    /// Lines can be picked one by one (file exists on both sides, working copy diff).
    pub selectable: bool,
    pub highlights: Vec<Vec<(Range<usize>, HighlightStyle)>>,
    pub selected: BTreeSet<usize>,
    pub anchor: Option<usize>,
    pub longest: usize,
    /// Rows of the side-by-side view, and the part that changed inside each edited line.
    pub rows: Vec<diff_view::SplitRow>,
    pub words: Vec<Option<Range<usize>>>,
}

/// Which panel edge is being dragged.
#[derive(Clone, Copy, PartialEq)]
pub enum Side {
    Left,
    Right,
    /// Right edge of the changed files column.
    Files,
}

const SIDEBAR_W: f32 = 240.;
const PANEL_W: f32 = 380.;
const FILES_W: f32 = 360.;
const SIDEBAR_RANGE: (f32, f32) = (200., 440.);
const PANEL_RANGE: (f32, f32) = (300., 680.);
const FILES_RANGE: (f32, f32) = (280., 640.);
/// The diff next to the changed files keeps at least this much room.
const DIFF_MIN_W: f32 = 360.;
/// The history keeps at least this much room when panels grow.
const CENTER_MIN_W: f32 = 420.;

#[derive(Clone, Copy, PartialEq)]
pub enum LineOp {
    Stage,
    Unstage,
    Discard,
}

impl LineOp {
    /// Unstage and Discard apply the patch with `--reverse`: it must be built for that direction.
    pub fn reversed(self) -> bool {
        self != LineOp::Stage
    }
}

type After = Box<dyn FnOnce(&mut Rameau, &mut Window, &mut Context<Rameau>)>;

/// A git command to run in the background.
pub struct GitJob {
    what: &'static str,
    args: Vec<String>,
    dir: Option<PathBuf>,
    input: Option<String>,
    env: Vec<(String, String)>,
    done: Option<&'static str>,
    after: Option<After>,
    /// Only the working copy changes (staging, discarding): refresh the file lists, not the history.
    status_only: bool,
    /// Talks to a server: only these jobs get the GitHub token. Git hooks (pre-commit…) run
    /// with the job's environment, and a cloned project's hooks must not see the token.
    network: bool,
}

impl GitJob {
    pub fn new(what: &'static str, args: &[&str]) -> Self {
        Self::owned(what, args.iter().map(|s| s.to_string()).collect())
    }

    pub fn owned(what: &'static str, args: Vec<String>) -> Self {
        Self { what, args, dir: None, input: None, env: vec![], done: None, after: None, status_only: false, network: false }
    }

    pub fn done(mut self, msg: &'static str) -> Self {
        self.done = Some(msg);
        self
    }

    pub fn after(mut self, f: impl FnOnce(&mut Rameau, &mut Window, &mut Context<Rameau>) + 'static) -> Self {
        self.after = Some(Box::new(f));
        self
    }

    pub fn network(mut self) -> Self {
        self.network = true;
        self
    }

    pub fn status_only(mut self) -> Self {
        self.status_only = true;
        self
    }

    fn dir(mut self, dir: PathBuf) -> Self {
        self.dir = Some(dir);
        self
    }

    fn input(mut self, input: String) -> Self {
        self.input = Some(input);
        self
    }
}

pub struct Rameau {
    pub config: Config,
    pub page: Page,
    pub proj: Option<Project>,
    pub loading: bool,
    /// Project being opened: a reload meanwhile (e.g. window re-activated) must target it, not cancel it.
    pending: Option<PathBuf>,
    pub view: View,
    pub selection: Selection,
    pub details: Option<Details>,
    pub diff: Option<OpenDiff>,
    pub scroll: UniformListScrollHandle,
    pub diff_scroll: UniformListScrollHandle,
    /// First line of the commit message.
    pub summary: Entity<InputState>,
    /// Rest of the commit message, optional.
    pub description: Entity<InputState>,
    /// The commit button rewrites the last commit (`--amend`) instead of adding one.
    pub amend: bool,
    pub clone_url: Entity<InputState>,
    pub clone_parent: PathBuf,
    pub busy: Option<SharedString>,
    pub login: Login,
    /// Profile picture of the GitHub account.
    pub avatar: Option<Arc<Image>>,
    /// Repositories of the GitHub account (None: not asked yet), and the search among them.
    pub gh_repos: Option<Result<Vec<github::RemoteRepo>, String>>,
    pub repo_filter: Entity<InputState>,
    gh_task: Option<Task<()>>,
    /// History search: the field, what can be searched (read on the first search), the commits found.
    pub history_search: Entity<InputState>,
    /// The search field is shown (else a look-alike button: a GPUI text field in the history view
    /// keeps ~90 MB of GPU memory while visible, measured, cause not found in GPUI).
    pub search_open: bool,
    search_index: Option<Arc<Vec<repo::Found>>>,
    pub search_hits: Option<(Arc<Vec<repo::Found>>, Vec<usize>)>,
    search_task: Option<Task<()>>,
    /// Pull requests and CI results of the open project (GitHub projects, signed in).
    pub gh_info: Option<github::RepoInfo>,
    gh_info_task: Option<Task<()>>,
    /// Project, refs fingerprint and time of the last reading: not asked again for a minute.
    gh_info_at: Option<(PathBuf, u64, std::time::Instant)>,
    /// Authors' GitHub pictures by e-mail (lower case); None while loading or when there is none.
    pub avatars: std::collections::HashMap<String, Option<Arc<Image>>>,
    pub menu_target: Option<MenuTarget>,
    pub focus: FocusHandle,
    pub resizing: Option<Side>,
    /// Width left for the history, refreshed each frame (decides which columns fit).
    pub center_w: f32,
    /// Window width at the last frame: panel widths are fitted into it.
    pub viewport_w: f32,
    load_task: Option<Task<()>>,
    login_task: Option<Task<()>>,
    status_task: Option<Task<()>>,
    /// Bumped by each status refresh: a full load that started earlier must not overwrite its result.
    status_gen: u64,
    diff_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Rameau {
    pub fn new(config: Config, open: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let summary = cx.new(|cx| InputState::new(window, cx).placeholder("Résumé (obligatoire)"));
        let description = cx.new(|cx| InputState::new(window, cx).multi_line(true).placeholder("Description"));
        let clone_url = cx.new(|cx| InputState::new(window, cx).placeholder("https://github.com/utilisateur/projet.git"));
        let repo_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Rechercher un dépôt"));
        let history_search = cx.new(|cx| InputState::new(window, cx).placeholder("Rechercher un commit : message, auteur ou hash"));
        let mut subscriptions = vec![
            // Pick up changes made outside the app (editor, terminal) when coming back to it.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh_on_focus(window, cx);
                }
            }),
            cx.subscribe(&repo_filter, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&history_search, |this, _, ev: &InputEvent, cx| match ev {
                InputEvent::Change => this.run_search(cx),
                // Left empty: back to the button.
                InputEvent::Blur if this.search_hits.is_none() => {
                    this.search_open = false;
                    // Read again (~100 ms) at the next search rather than kept for nothing.
                    this.search_index = None;
                    cx.notify();
                }
                _ => {}
            }),
        ];
        // ⌘↩ (Ctrl+↩) commits from either field.
        for input in [&summary, &description] {
            subscriptions.push(cx.subscribe_in(input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { secondary: true } = ev {
                    this.commit(window, cx);
                }
            }));
        }
        cx.spawn(async move |this, cx| {
            let account = cx.background_executor().spawn(async { github::load() }).await;
            this.update(cx, |this, cx| {
                if let Some(a) = account {
                    this.signed_in(a, cx);
                }
            })
            .ok();
        })
        .detach();

        let focus = cx.focus_handle();
        window.focus(&focus);
        let clone_parent = config.last.as_ref().and_then(|p| p.parent()).map_or_else(config::home, |p| p.to_owned());
        let mut this = Self {
            config,
            page: Page::Import,
            proj: None,
            loading: false,
            pending: None,
            view: View::History,
            selection: Selection::None,
            details: None,
            diff: None,
            scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
            summary,
            description,
            amend: false,
            clone_url,
            clone_parent,
            busy: None,
            login: Login::Out,
            avatar: None,
            gh_repos: None,
            repo_filter,
            gh_task: None,
            history_search,
            search_open: false,
            search_index: None,
            search_hits: None,
            search_task: None,
            gh_info: None,
            gh_info_task: None,
            gh_info_at: None,
            avatars: Default::default(),
            menu_target: None,
            focus,
            resizing: None,
            center_w: 1000.,
            viewport_w: 1320.,
            load_task: None,
            login_task: None,
            status_task: None,
            status_gen: 0,
            diff_task: None,
            _subscriptions: subscriptions,
        };
        if let Some(path) = open.or_else(|| this.config.last.clone()) {
            this.page = Page::Repo;
            this.load_project(path, window, cx);
        }
        this
    }

    pub fn dir(&self) -> Option<PathBuf> {
        self.proj.as_ref().map(|p| p.path.clone())
    }

    pub fn history(&self) -> Option<&History> {
        self.proj.as_ref().map(|p| &p.history)
    }

    fn token(&self) -> Option<String> {
        match &self.login {
            Login::In(a) => Some(a.token.clone()),
            _ => None,
        }
    }

    /// A push or pull the server refused for lack of credentials: what to do, then git's own words.
    fn login_needed(&self, what: &str, git_msg: String, window: &mut Window, cx: &mut Context<Self>) {
        let signed_in = matches!(self.login, Login::In(_));
        let advice = if signed_in {
            "Le serveur a refusé ton compte GitHub connecté : vérifie que tu as le droit d'écrire sur ce dépôt."
        } else {
            "Le serveur demande qui tu es. Connecte-toi avec GitHub, ou configure tes identifiants git \
             habituels (clé SSH, gestionnaire d'identifiants), puis réessaie."
        };
        let mut n = Notification::warning(format!("{advice}\n\n{git_msg}")).title(format!("{what} : identification requise")).autohide(false);
        if !signed_in {
            let weak = cx.weak_entity();
            n = n.action(move |_, _, _| {
                let weak = weak.clone();
                Button::new("notify-login").primary().small().label("Se connecter avec GitHub").on_click(move |_, window, cx| {
                    weak.update(cx, |this, cx| this.login(window, cx)).ok();
                })
            });
        }
        window.push_notification(n, cx);
    }

    pub fn notify_error(&self, title: &str, msg: impl Into<SharedString>, window: &mut Window, cx: &mut Context<Self>) {
        // Errors stay until closed: git messages are often several lines long.
        window.push_notification(Notification::error(msg).title(title.to_owned()).autohide(false), cx);
    }

    /// Opens (or re-reads) a project off the UI thread.
    pub fn load_project(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let switching = self.proj.as_ref().is_none_or(|p| p.path != path);
        if switching {
            self.loading = true;
            self.pending = Some(path.clone());
            cx.notify();
        }
        let p = path.clone();
        let status_at_start = self.status_gen;
        // A reload keeps the commits already scrolled through.
        let shown = if switching { 0 } else { self.history().map_or(0, |h| h.commits.len()) };
        let task = cx.background_executor().spawn(async move {
            let history = repo::load(&mut Repository::open(&p)?, shown)?;
            // A fresh handle: the one that walked the history caches every commit it read.
            Ok::<_, git2::Error>((Repository::open(&p)?, history))
        });
        // Replacing the task cancels a load still in flight.
        self.load_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                this.pending = None;
                match result {
                    Ok((repo, mut history)) => {
                        let stale = !switching && this.status_gen != status_at_start;
                        keep_fresher_changes(&mut history, this.proj.as_mut(), stale);
                        // The selected commit may be gone (rebase, amend): drop the stale details.
                        if let Selection::Commit(id) = this.selection
                            && !history.commits.iter().any(|c| c.id == id)
                            && !history.stashes.iter().any(|s| s.id == id)
                        {
                            this.selection = Selection::None;
                            this.details = None;
                        }
                        if switching {
                            this.selection = Selection::None;
                            this.details = None;
                            this.close_diff();
                            this.scroll = UniformListScrollHandle::new();
                            // Something to commit: that's what the user most likely came for.
                            this.view = if history.changes.is_empty() { View::History } else { View::Changes };
                            window.set_window_title(&format!("{} — Rameau", history.name));
                            this.config.add(&path);
                            this.config.save();
                            this.page = Page::Repo;
                        }
                        this.proj = Some(Project { path, repo, history });
                        // The history may have changed: search it again, from a fresh index.
                        this.search_index = None;
                        if switching {
                            this.history_search.update(cx, |s, cx| s.set_value("", window, cx));
                            this.search_open = false;
                        }
                        this.run_search(cx);
                        this.refresh_github(cx);
                        this.want_avatars(cx);
                        if switching && this.view == View::Changes {
                            this.open_first_file(cx);
                        }
                        this.refresh_diff(cx);
                    }
                    Err(e) => {
                        let msg = format!("{} : {}", path.display(), e.message());
                        this.notify_error("Impossible d'ouvrir le projet", msg, window, cx);
                        if this.proj.is_none() {
                            this.page = Page::Import;
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Re-reads only the changed files (cheap even with a huge history), off the UI thread.
    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let Some(git_dir) = self.proj.as_ref().map(|p| p.repo.path().to_owned()) else { return };
        self.status_gen += 1;
        let task = cx.background_executor().spawn(async move {
            let repo = Repository::open(&git_dir)?;
            let changes = repo::changes(&repo)?;
            Ok::<_, git2::Error>((repo, changes))
        });
        self.status_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match (result, this.proj.as_mut()) {
                    // The fresh repository also sees the new index for the diffs.
                    (Ok((repo, changes)), Some(p)) if p.repo.path() == repo.path() => {
                        p.repo = repo;
                        p.history.changes = changes;
                    }
                    (Err(e), _) => eprintln!("rameau: {}", e.message()),
                    _ => {}
                }
                this.refresh_diff(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Back in the window: files may have changed (editor), the history rarely did. Reload the
    /// history only when its refs moved (commit, fetch… in a terminal), else just the file lists.
    fn refresh_on_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.proj else { return self.reload(window, cx) };
        if self.pending.is_some() {
            return self.reload(window, cx);
        }
        let (git_dir, known) = (p.repo.path().to_owned(), p.history.fingerprint);
        let task = cx.background_executor().spawn(async move {
            Repository::open(&git_dir).map(|r| repo::fingerprint(&r) != known).unwrap_or(true)
        });
        cx.spawn_in(window, async move |this, cx| {
            let moved = task.await;
            this.update_in(cx, |this, window, cx| {
                if moved {
                    this.reload(window, cx)
                } else {
                    this.refresh_status(cx);
                    // CI results change without any ref moving.
                    this.refresh_github(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.pending.clone().or_else(|| self.dir()) {
            self.load_project(path, window, cx);
        }
    }

    pub fn remove_current_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.proj.take() else { return };
        self.config.remove(&p.path);
        self.config.save();
        self.close_diff();
        self.selection = Selection::None;
        match self.config.last.clone() {
            Some(next) => self.load_project(next, window, cx),
            None => self.page = Page::Import,
        }
        cx.notify();
    }

    pub fn select_commit(&mut self, id: Oid, cx: &mut Context<Self>) {
        if self.proj.is_none() {
            return;
        }
        self.selection = Selection::Commit(id);
        self.close_diff();
        let Some(p) = &self.proj else { return };
        // An unreadable commit (partial clone, broken object) says so in the panel.
        self.details = Some(repo::details(&p.repo, id).unwrap_or_else(|e| repo::Details::unreadable(id, e.message())));
        cx.notify();
    }

    pub fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        if self.view != view {
            self.view = view;
            self.close_diff();
            if view == View::Changes {
                self.open_first_file(cx);
            }
        }
        cx.notify();
    }

    /// Changes view: show the first file's diff rather than an empty area.
    fn open_first_file(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.history().and_then(|h| h.changes.files.first()) else { return };
        let (source, path) = (f.diff_source(), f.path.clone());
        self.open_diff(source, path, false, cx);
    }

    /// Pictures of the authors of commits about to be read.
    fn want_avatars_later(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| this.update(cx, |this, cx| this.want_avatars(cx)).ok()).detach();
    }

    /// Reads the commits of the history up to `upto` (they are read a page at a time, when shown).
    pub fn load_commits(&mut self, upto: usize) {
        let Some(p) = &mut self.proj else { return };
        if upto > p.history.commits.len()
            && let Err(e) = repo::load_more(&p.repo, &mut p.history, upto)
        {
            eprintln!("rameau: {}", e.message());
        }
    }

    /// Shows the history search field, ready to type in.
    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.history_search.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// Filters the history with the search field (the index of every commit is read the first time).
    pub fn run_search(&mut self, cx: &mut Context<Self>) {
        let query = self.history_search.read(cx).value().trim().to_owned();
        if query.is_empty() {
            self.search_hits = None;
            self.search_task = None;
            return cx.notify();
        }
        if let Some(index) = self.search_index.clone() {
            let hits = repo::search(&index, &query);
            self.search_hits = Some((index, hits));
            return cx.notify();
        }
        let Some(p) = &self.proj else { return };
        let (dir, with_head) = (p.path.clone(), p.history.has_commits);
        let task = cx.background_executor().spawn(async move { repo::search_index(&dir, with_head) });
        // Replacing the task drops a reading still in flight (another project opened meanwhile).
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let index = task.await;
            this.update(cx, |this, cx| match index {
                Ok(index) => {
                    this.search_index = Some(Arc::new(index));
                    this.run_search(cx);
                }
                Err(e) => eprintln!("rameau: recherche : {e}"),
            })
            .ok();
        }));
    }

    /// ↑/↓: walk the history.
    pub fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.want_avatars_later(cx);
        if self.view != View::History {
            return;
        }
        let Some(h) = self.history() else { return };
        let current = match self.selection {
            Selection::Commit(id) => h.commits.iter().position(|c| c.id == id).map(|i| i as isize),
            Selection::None => None,
        };
        let next = current.map_or(0, |i| i + delta);
        if next < 0 {
            return;
        }
        let next = next as usize;
        self.load_commits(next + repo::PAGE);
        let Some(id) = self.history().and_then(|h| h.commits.get(next)).map(|c| c.id) else { return };
        self.scroll.scroll_to_item(next, ScrollStrategy::Top);
        self.select_commit(id, cx);
    }

    /// Shows the commit at `row` of the history (a branch tip clicked in the sidebar).
    pub fn reveal_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.want_avatars_later(cx);
        self.view = View::History;
        self.close_diff();
        self.load_commits(row + repo::PAGE);
        let Some(id) = self.history().and_then(|h| h.commits.get(row)).map(|c| c.id) else { return };
        self.scroll.scroll_to_item(row, ScrollStrategy::Center);
        self.select_commit(id, cx);
    }

    /// Computes and highlights a file diff off the UI thread (a huge generated file would
    /// freeze the window otherwise). `keep_scroll` keeps the position when refreshing.
    pub fn open_diff(&mut self, source: DiffSource, path: String, keep_scroll: bool, cx: &mut Context<Self>) {
        let Some(git_dir) = self.proj.as_ref().map(|p| p.repo.path().to_owned()) else { return };
        let task = cx.background_executor().spawn(async move {
            let repo = Repository::open(&git_dir)?;
            let (lines, partial) = match repo::file_diff(&repo, &source, &path) {
                Ok(diff) => diff,
                // Shown in the diff view: the click must not look ignored.
                Err(e) => (vec![repo::DiffLine::info(&format!("Impossible d'afficher ce fichier : {}", e.message()))], false),
            };
            let highlights = diff_view::highlight(&repo, &source, &path, &lines);
            let longest = lines.iter().enumerate().max_by_key(|(_, l)| l.text.len()).map_or(0, |(i, _)| i);
            let selectable = partial && !matches!(source, DiffSource::Commit(_));
            let rows = diff_view::split_rows(&lines);
            let words = diff_view::word_changes(&lines, &rows);
            Ok::<_, git2::Error>(OpenDiff {
                source,
                path,
                lines,
                selectable,
                highlights,
                selected: BTreeSet::new(),
                anchor: None,
                longest,
                rows,
                words,
            })
        });
        // Replacing the task drops a diff still loading (another file was clicked meanwhile).
        self.diff_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(diff) => {
                        if !keep_scroll {
                            this.diff_scroll = UniformListScrollHandle::new();
                        }
                        this.diff = Some(diff);
                    }
                    Err(e) => eprintln!("rameau: {}", e.message()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn close_diff(&mut self) {
        self.diff = None;
        // A diff still loading must not reopen once closed.
        self.diff_task = None;
    }

    /// After a reload, keep the open working-copy diff in sync: it follows the file
    /// from "unstaged" to "staged" (and back), and closes when the file is clean.
    pub fn refresh_diff(&mut self, cx: &mut Context<Self>) {
        let Some(d) = &self.diff else { return };
        let Some(ch) = self.history().map(|h| &h.changes) else { return };
        let listed = |l: &[(char, String)]| l.iter().any(|(_, p)| *p == d.path);
        let has = |s: &DiffSource| match s {
            DiffSource::Commit(_) => true,
            DiffSource::Staged => listed(&ch.staged),
            DiffSource::Unstaged => listed(&ch.unstaged) || ch.conflicted.contains(&d.path),
        };
        let other = match &d.source {
            DiffSource::Staged => DiffSource::Unstaged,
            DiffSource::Unstaged => DiffSource::Staged,
            commit => commit.clone(),
        };
        // The current diff stays on screen until the new one is ready: no flicker.
        match [d.source.clone(), other].into_iter().find(has) {
            Some(source) => {
                let path = d.path.clone();
                self.open_diff(source, path, true, cx);
            }
            None => self.close_diff(),
        }
    }

    /// Runs a git command in the background, reports failures, then reloads.
    pub fn run(&mut self, job: GitJob, window: &mut Window, cx: &mut Context<Self>) {
        // shortcut: one command at a time, clicks while busy are ignored; add a queue if that bites.
        if self.busy.is_some() {
            return;
        }
        let Some(dir) = job.dir.clone().or_else(|| self.dir()) else { return };
        self.busy = Some(format!("{}…", job.what).into());
        cx.notify();
        let token = if job.network { self.token() } else { None };
        let GitJob { what, args, input, env, done, after, status_only, network, .. } = job;
        let task = cx.background_executor().spawn(async move {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            git::exec(&dir, &a, input.as_deref(), token.as_deref(), &env)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(_) => {
                        if let Some(msg) = done {
                            window.push_notification(Notification::success(msg), cx);
                        }
                        if let Some(after) = after {
                            after(this, window, cx);
                        }
                    }
                    // A merge/rebase stopping on conflicts is expected, the banner takes over from here.
                    Err(e) if e.contains("CONFLICT") => window.push_notification(
                        Notification::warning("Des conflits sont à résoudre avant de continuer.").title(format!("{what} : conflits")),
                        cx,
                    ),
                    Err(e) if network && needs_login(&e) => this.login_needed(what, e, window, cx),
                    Err(e) => this.notify_error(&format!("{what} : échec"), e, window, cx),
                }
                if status_only {
                    this.refresh_status(cx);
                } else {
                    this.reload(window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks before running a destructive job.
    pub fn confirm(&mut self, title: String, message: String, ok: &'static str, job: GitJob, window: &mut Window, cx: &mut Context<Self>) {
        let job = Rc::new(RefCell::new(Some(job)));
        let weak = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (job, weak) = (job.clone(), weak.clone());
            let dialog = dialog.title(title.clone()).child(message.clone());
            ui::with_footer(dialog, ok, true, move |window, cx| {
                if let Some(job) = job.borrow_mut().take() {
                    weak.update(cx, |this, cx| this.run(job, window, cx)).ok();
                }
                true
            })
        });
    }

    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(h) = self.history() else { return };
        // Amending may only change the message.
        if h.changes.staged.is_empty() && !self.amend {
            window.push_notification(Notification::warning("Coche au moins un fichier à inclure dans le commit."), cx);
            return;
        }
        let summary = self.summary.read(cx).value().trim().to_owned();
        if summary.is_empty() {
            window.push_notification(Notification::warning("Écris un résumé du commit."), cx);
            self.summary.update(cx, |s, cx| s.focus(window, cx));
            return;
        }
        let description = self.description.read(cx).value().trim().to_owned();
        let msg = if description.is_empty() { summary } else { format!("{summary}\n\n{description}") };
        // A fresh Git (Windows, Linux) has no author: git would refuse the commit in English.
        let signed = self.proj.as_ref().is_some_and(|p| Repository::open(p.repo.path()).and_then(|r| r.signature()).is_ok());
        if !signed {
            return self.identity_dialog(true, window, cx);
        }
        let amend = self.amend && self.history().is_some_and(rewritable);
        let mut args = vec!["commit".into()];
        if amend {
            args.push("--amend".into());
        }
        args.extend(["-m".into(), msg]);
        let job = GitJob::owned(if amend { "Modification du commit" } else { "Commit" }, args).after(|this, window, cx| {
            this.amend = false;
            for input in [this.summary.clone(), this.description.clone()] {
                input.update(cx, |s, cx| s.set_value("", window, cx));
            }
        });
        self.run(job, window, cx);
    }

    /// "Modifier le dernier commit": its message comes back in the fields, to be edited.
    pub fn toggle_amend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = !self.amend;
        let message = self.amend.then(|| self.head_message()).flatten();
        let empty = self.summary.read(cx).value().trim().is_empty() && self.description.read(cx).value().trim().is_empty();
        if let Some((summary, description)) = message.filter(|_| empty) {
            self.summary.update(cx, |s, cx| s.set_value(summary, window, cx));
            self.description.update(cx, |s, cx| s.set_value(description, window, cx));
        }
        cx.notify();
    }

    /// Summary and description of the HEAD commit.
    fn head_message(&self) -> Option<(String, String)> {
        let c = self.proj.as_ref()?.repo.head().ok()?.peel_to_commit().ok()?;
        let msg = c.message().unwrap_or("").trim().to_owned();
        let (summary, rest) = msg.split_once('\n').unwrap_or((&msg, ""));
        Some((summary.trim().to_owned(), rest.trim().to_owned()))
    }

    /// Takes the last commit back: its changes stay, checked, and its message returns to the fields.
    pub fn undo_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.proj else { return };
        if !rewritable(&p.history) {
            return;
        }
        let Some(parents) = p.repo.head().ok().and_then(|h| h.peel_to_commit().ok()).map(|c| c.parent_count()) else { return };
        if parents == 0 {
            let msg = "C'est le premier commit du dépôt : il n'y a rien avant lui où revenir.";
            return window.push_notification(Notification::warning(msg), cx);
        }
        let message = self.head_message();
        let job = GitJob::new("Annulation du commit", &["reset", "--soft", "HEAD~1"]).done("Commit annulé : ses modifications sont de nouveau cochées").after(
            move |this, window, cx| {
                this.view = View::Changes;
                if let Some((summary, description)) = message {
                    this.summary.update(cx, |s, cx| s.set_value(summary, window, cx));
                    this.description.update(cx, |s, cx| s.set_value(description, window, cx));
                }
            },
        );
        self.run(job, window, cx);
    }

    /// Asks the name and e-mail that sign commits and saves them in the user's git config;
    /// `then_commit`: asked by a commit, which goes on once they are saved.
    pub fn identity_dialog(&mut self, then_commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (current_name, current_email) = current_identity();
        let (suggested_name, suggested_email) = identity_suggestion(&self.login);
        let name0 = if current_name.is_empty() { suggested_name } else { current_name };
        let email0 = if current_email.is_empty() { suggested_email.clone() } else { current_email };
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Prénom Nom").default_value(name0));
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("toi@exemple.com").default_value(email0));
        let (weak, focus) = (cx.weak_entity(), name.clone());
        window.open_dialog(cx, move |dialog, _, _| {
            let (name, email, weak) = (name.clone(), email.clone(), weak.clone());
            let p = theme::pal();
            let body = div()
                .flex()
                .flex_col()
                .gap_3()
                .child(div().text_sm().text_color(theme::c(p.muted)).child(
                    "Git signe chaque commit avec un nom et une adresse e-mail, visibles dans l'historique. \
                     Ils sont enregistrés une fois pour tous tes projets.",
                ))
                .child(Input::new(&name))
                .child(Input::new(&email))
                .when(!suggested_email.is_empty(), |d| {
                    d.child(div().text_xs().text_color(theme::c(p.faint)).child(format!(
                        "Pour garder ton adresse privée, GitHub te propose : {suggested_email}"
                    )))
                });
            let ok = if then_commit { "Enregistrer et committer" } else { "Enregistrer" };
            ui::with_footer(dialog.title("Qui signe les commits ?").child(body), ok, false, move |window, cx| {
                let (n, e) = (name.read(cx).value().trim().to_owned(), email.read(cx).value().trim().to_owned());
                if let Err(msg) = save_identity(&n, &e, &[]) {
                    window.push_notification(Notification::warning(msg), cx);
                    return false;
                }
                if then_commit {
                    weak.update(cx, |this, cx| this.commit(window, cx)).ok();
                } else {
                    window.push_notification(Notification::success("Identité enregistrée pour tes prochains commits"), cx);
                }
                true
            })
        });
        cx.defer_in(window, move |_, window, cx| focus.update(cx, |s, cx| s.focus(window, cx)));
    }

    pub fn pull(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.proj else { return };
        self.run(GitJob::new("Pull", &pull_args(&p.repo)).network().done("Pull terminé"), window, cx);
    }

    pub fn push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.proj else { return };
        let job = if p.history.ahead_behind.is_some() {
            GitJob::new("Push", &["push"])
        } else {
            // First push of this branch: publish it and set its upstream.
            GitJob::new("Push", &["push", "-u", &push_remote(&p.repo), "HEAD"])
        };
        self.run(job.network().done("Push terminé"), window, cx);
    }

    /// Stages, unstages or discards the selected lines of the open diff (or one hunk).
    pub fn apply_lines(&mut self, op: LineOp, hunk: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = &self.diff else { return };
        let hunk_end = hunk.map(|h| (h + 1..d.lines.len()).find(|&i| d.lines[i].kind == LineKind::Hunk).unwrap_or(d.lines.len()));
        let selected = |i: usize| match (hunk, hunk_end) {
            (Some(h), Some(end)) => i > h && i < end,
            _ => d.selected.contains(&i),
        };
        let Some(patch) = patch::build(&d.path, &d.lines, selected, op.reversed()) else { return };
        let (what, args): (&'static str, &[&str]) = match op {
            LineOp::Stage => ("Ajout des lignes", &["apply", "--cached", "--recount", "-"]),
            LineOp::Unstage => ("Retrait des lignes", &["apply", "--cached", "--recount", "--reverse", "-"]),
            LineOp::Discard => ("Abandon des lignes", &["apply", "--recount", "--reverse", "-"]),
        };
        let job = GitJob::new(what, args).input(patch).status_only();
        if op == LineOp::Discard {
            let n = match hunk {
                Some(_) => "ce bloc".to_owned(),
                None => format!("{} ligne(s)", d.selected.len()),
            };
            let msg = format!("Les modifications de {n} dans {} seront perdues définitivement.", d.path);
            self.confirm("Abandonner des modifications ?".into(), msg, "Abandonner les modifications", job, window, cx);
        } else {
            self.run(job, window, cx);
        }
    }

    pub fn discard_file(&mut self, path: String, untracked: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (job, msg) = if untracked {
            (GitJob::owned("Suppression", vec!["clean".into(), "-f".into(), "--".into(), git::literal(&path)]).status_only(), format!("Le nouveau fichier {path} sera supprimé."))
        } else {
            (
                GitJob::owned("Annulation", vec!["restore".into(), "--".into(), git::literal(&path)]).status_only(),
                format!("Les modifications non indexées de {path} seront perdues définitivement."),
            )
        };
        self.confirm("Abandonner les modifications ?".into(), msg, "Abandonner les modifications", job, window, cx);
    }

    pub fn delete_branch(&mut self, name: String, kind: RefKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.proj else { return };
        if kind == RefKind::Remote {
            let Some((remote, branch)) = name.split_once('/') else { return };
            let job = GitJob::new("Suppression de la branche distante", &["push", remote, "--delete", branch]).network();
            let msg = format!("La branche {branch} sera supprimée du serveur {remote}, pour tout le monde.");
            self.confirm(format!("Supprimer {name} ?"), msg, "Supprimer sur le serveur", job, window, cx);
            return;
        }
        // Commits only reachable from this branch would be lost: say so.
        let merged = (|| {
            let tip = p.repo.find_branch(&name, git2::BranchType::Local).ok()?.get().target()?;
            let head = p.repo.head().ok()?.target()?;
            Some(tip == head || p.repo.graph_descendant_of(head, tip).ok()?)
        })()
        .unwrap_or(false);
        let msg = if merged {
            format!("La branche {name} est déjà intégrée à {} : aucun commit ne sera perdu.", p.history.head)
        } else {
            format!("La branche {name} contient des commits absents de {} : ils ne seront plus visibles.", p.history.head)
        };
        let job = GitJob::new("Suppression de la branche", &["branch", "-D", &name]);
        self.confirm(format!("Supprimer {name} ?"), msg, "Supprimer", job, window, cx);
    }

    pub fn new_branch_dialog(&mut self, at: Oid, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("nom-de-la-branche"));
        let weak = cx.weak_entity();
        let focus = input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, weak) = (input.clone(), weak.clone());
            let dialog = dialog.title("Nouvelle branche").child(Input::new(&input));
            ui::with_footer(dialog, "Créer la branche", false, move |window, cx| {
                let name = input.read(cx).value().trim().to_owned();
                if name.is_empty() {
                    return false;
                }
                let job = GitJob::owned("Création de la branche", vec!["switch".into(), "-c".into(), name, at.to_string()]);
                weak.update(cx, |this, cx| this.run(job, window, cx)).ok();
                true
            })
        });
        // The dialog takes focus when it opens: give it to the field right after.
        cx.defer_in(window, move |_, window, cx| focus.update(cx, |s, cx| s.focus(window, cx)));
    }

    pub fn interactive_rebase(&mut self, from: Oid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.proj else { return };
        let (base, commits) = match repo::rebase_range(&p.repo, from) {
            Ok(r) => r,
            Err(e) => return window.push_notification(Notification::warning(e).title("Rebase interactif impossible"), cx),
        };
        let editor = cx.new(|_| RebaseEditor::new(base, commits));
        let weak = cx.weak_entity();
        // The list is fixed now: a commit made before "Lancer" would be missing from it, and git drops
        // commits missing from the list.
        let (git_dir, head) = (p.repo.path().to_owned(), p.repo.head().ok().and_then(|h| h.target()));
        window.open_dialog(cx, move |dialog, _, _| {
            let (editor, weak) = (editor.clone(), weak.clone());
            let dialog = dialog.title("Rebase interactif").w(px(760.)).child(editor.clone());
            let git_dir = git_dir.clone();
            ui::with_footer(dialog, "Lancer le rebase", false, move |window, cx| {
                if head.is_none_or(|h| repo::head_moved(&git_dir, h)) {
                    let msg = "Un commit a été fait pendant que cette fenêtre était ouverte. Ferme-la et relance le rebase interactif.";
                    window.push_notification(Notification::warning(msg).title("L'historique a changé"), cx);
                    return false;
                }
                // Written in the project's .git folder, the user's own: a shared temp folder lets
                // another account on the machine plant a file there first.
                match editor.read(cx).plan(&git_dir, cx) {
                Err(e) => {
                    window.push_notification(Notification::error(e), cx);
                    false
                }
                Ok((base, todo)) => {
                    weak.update(cx, |this, cx| this.run_rebase(base, todo, window, cx)).ok();
                    true
                }
                }
            })
        });
    }

    fn run_rebase(&mut self, base: Option<Oid>, todo: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(git_dir) = self.proj.as_ref().map(|p| p.repo.path().to_owned()) else { return };
        let file = git_dir.join("rameau-todo.txt");
        if let Err(e) = std::fs::write(&file, todo) {
            return self.notify_error("Rebase interactif", e.to_string(), window, cx);
        }
        let (args, env) = crate::rebase::command(base, &file);
        let mut job = GitJob::owned("Rebase interactif", args).done("Rebase terminé");
        job.env = env;
        self.run(job, window, cx);
    }

    pub fn login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client_id) = github::CLIENT_ID else {
            let msg = "Cette version a été compilée sans RAMEAU_GITHUB_CLIENT_ID (voir README).";
            return self.notify_error("Connexion GitHub indisponible", msg, window, cx);
        };
        let start = cx.background_executor().spawn(async move { github::start(client_id) });
        // shortcut: cancelling drops this task but the polling thread runs until the code expires (15 min, then exits).
        self.login_task = Some(cx.spawn_in(window, async move |this, cx| {
            let code = match start.await {
                Ok(code) => code,
                Err(e) => {
                    this.update_in(cx, |this, window, cx| this.notify_error("Connexion GitHub", e, window, cx)).ok();
                    return;
                }
            };
            this.update(cx, |this, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(code.user_code.clone()));
                cx.open_url(&code.verification_uri);
                this.login = Login::Waiting(code.user_code.clone());
                cx.notify();
            })
            .ok();
            let result = cx.background_executor().spawn(async move { github::wait_for_token(client_id, &code) }).await;
            // Reached only if the sign-in wasn't cancelled meanwhile ("Annuler" drops this task):
            // a token approved in the browser after cancelling is never stored.
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(a) => {
                        let token = a.token.clone();
                        cx.background_executor()
                            .spawn(async move {
                                // Not fatal: the sign-in then just lasts for this session.
                                if let Err(e) = github::save(&token) {
                                    eprintln!("rameau: token non enregistré : {e}");
                                }
                            })
                            .detach();
                        window.push_notification(Notification::success(format!("Connecté en tant que {}", a.login)), cx);
                        this.signed_in(a, cx);
                    }
                    Err(e) => {
                        this.notify_error("Connexion GitHub", e, window, cx);
                        this.login = Login::Out;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn logout(&mut self, cx: &mut Context<Self>) {
        if let Login::In(_) = &self.login {
            github::forget();
        }
        self.login = Login::Out;
        self.login_task = None;
        self.avatar = None;
        self.gh_repos = None;
        self.gh_task = None;
        self.gh_info = None;
        self.gh_info_task = None;
        cx.notify();
    }

    /// Signed in: fetches the profile picture and the list of repositories in the background.
    fn signed_in(&mut self, a: github::Account, cx: &mut Context<Self>) {
        if let Some(url) = a.avatar_url.clone() {
            cx.spawn(async move |this, cx| {
                let pic = cx.background_executor().spawn(async move { github::avatar(&url) }).await;
                this.update(cx, |this, cx| {
                    // Still the same sign-in (not logged out meanwhile).
                    if let (Some((bytes, format)), Login::In(_)) = (pic, &this.login) {
                        this.avatar = Some(Arc::new(Image::from_bytes(format, bytes)));
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        self.login = Login::In(a);
        self.fetch_repos(cx);
        self.gh_info_at = None;
        self.refresh_github(cx);
        cx.notify();
    }

    /// Reads the pull requests and CI results of the open project in the background, when it lives on
    /// github.com and the user is signed in. At most once a minute while its refs stay the same.
    pub fn refresh_github(&mut self, cx: &mut Context<Self>) {
        let (Some(token), Some(p)) = (self.token(), &self.proj) else { return };
        let remote = push_remote(&p.repo);
        let slug = p.repo.find_remote(&remote).ok().and_then(|r| r.url().and_then(github::repo_slug));
        let Some(slug) = slug else {
            self.gh_info = None;
            return;
        };
        let fresh = |(path, fp, at): &(PathBuf, u64, std::time::Instant)| {
            *path == p.path && *fp == p.history.fingerprint && at.elapsed() < std::time::Duration::from_secs(60)
        };
        if self.gh_info_at.as_ref().is_some_and(fresh) {
            return;
        }
        if self.gh_info.as_ref().is_some_and(|i| i.slug != slug) {
            self.gh_info = None;
        }
        self.gh_info_at = Some((p.path.clone(), p.history.fingerprint, std::time::Instant::now()));
        // Local branches as github.com has them: their CI ran on the pushed commit.
        let branches: Vec<(String, String)> = p
            .history
            .branches
            .iter()
            .filter(|b| b.kind != RefKind::Remote)
            .filter_map(|b| {
                let pushed = p.repo.find_reference(&format!("refs/remotes/{remote}/{}", b.name)).ok()?.target()?;
                Some((b.name.clone(), pushed.to_string()))
            })
            .collect();
        let task = cx.background_executor().spawn(async move { github::repo_info(&token, &slug, branches) });
        self.gh_info_task = Some(cx.spawn(async move |this, cx| {
            let info = task.await;
            this.update(cx, |this, cx| {
                match info {
                    Ok(info) => {
                        this.gh_info = Some(info);
                        this.want_avatars(cx);
                    }
                    Err(e) => eprintln!("rameau: GitHub : {e}"),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Fetches the GitHub pictures of the authors of the commits read so far: from their "noreply"
    /// address, or from what GitHub said about the recent commits. Each picture is asked once.
    pub fn want_avatars(&mut self, cx: &mut Context<Self>) {
        // shortcut: at most 300 pictures (~5 MB); the next authors keep their initials.
        const MAX: usize = 300;
        let Some(p) = &self.proj else { return };
        let known = self.gh_info.as_ref().map(|i| &i.avatars);
        let mut todo: Vec<(String, String)> = Vec::new();
        for c in &p.history.commits {
            if self.avatars.len() + todo.len() >= MAX {
                break;
            }
            let email = c.email.to_lowercase();
            if self.avatars.contains_key(&email) || todo.iter().any(|(e, _)| *e == email) {
                continue;
            }
            if let Some(url) = github::noreply_avatar(&email).or_else(|| known.and_then(|k| k.get(&email).cloned())) {
                todo.push((email, url));
            }
        }
        if todo.is_empty() {
            return;
        }
        for (email, _) in &todo {
            self.avatars.insert(email.clone(), None);
        }
        let task = cx.background_executor().spawn(async move {
            // 8 at a time: quick without opening dozens of connections at once.
            todo.chunks(8)
                .flat_map(|chunk| {
                    std::thread::scope(|s| {
                        let handles: Vec<_> = chunk.iter().map(|(email, url)| s.spawn(move || (email.clone(), github::avatar(url)))).collect();
                        handles.into_iter().filter_map(|h| h.join().ok()).collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let pictures = task.await;
            this.update(cx, |this, cx| {
                for (email, picture) in pictures {
                    if let Some((bytes, format)) = picture {
                        this.avatars.insert(email, Some(Arc::new(Image::from_bytes(format, bytes))));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// An author's GitHub picture, when it is known and loaded.
    pub fn avatar_of(&self, email: &str) -> Option<Arc<Image>> {
        self.avatars.get(&email.to_lowercase())?.clone()
    }

    /// Pull request open for `branch`, if any.
    pub fn pull_request(&self, branch: &str) -> Option<&github::PullRequest> {
        self.gh_info.as_ref()?.pulls.iter().find(|p| p.branch == branch)
    }

    /// (Re)reads the account's repositories for the import page.
    pub fn fetch_repos(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.token() else { return };
        self.gh_repos = None;
        let task = cx.background_executor().spawn(async move { github::repos(&token) });
        // Replacing the task drops a listing still in flight (logout, refresh).
        self.gh_task = Some(cx.spawn(async move |this, cx| {
            let repos = task.await;
            this.update(cx, |this, cx| {
                this.gh_repos = Some(repos);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn widths(&self) -> (f32, f32) {
        let chosen = (self.config.sidebar_width.unwrap_or(SIDEBAR_W), self.config.panel_width.unwrap_or(PANEL_W));
        fit_widths(self.viewport_w, chosen)
    }

    pub fn sidebar_w(&self) -> f32 {
        self.widths().0
    }

    pub fn panel_w(&self) -> f32 {
        self.widths().1
    }

    /// Changed files column: what the user chose, leaving the diff its minimum.
    pub fn files_w(&self) -> f32 {
        let room = self.viewport_w - self.sidebar_w() - DIFF_MIN_W;
        self.config.files_width.unwrap_or(FILES_W).min(room).max(FILES_RANGE.0)
    }

    /// Follows the mouse while a panel edge is dragged (`x` from the window's left edge).
    pub fn resize(&mut self, side: Side, x: f32, window_w: f32, cx: &mut Context<Self>) {
        let (w, min, max) = match side {
            Side::Left => (x, SIDEBAR_RANGE.0, SIDEBAR_RANGE.1.min(window_w - self.panel_w() - CENTER_MIN_W)),
            Side::Right => (window_w - x, PANEL_RANGE.0, PANEL_RANGE.1.min(window_w - self.sidebar_w() - CENTER_MIN_W)),
            Side::Files => (x - self.sidebar_w(), FILES_RANGE.0, FILES_RANGE.1.min(window_w - self.sidebar_w() - DIFF_MIN_W)),
        };
        let w = w.clamp(min, max.max(min)).round();
        match side {
            Side::Left => self.config.sidebar_width = Some(w),
            Side::Right => self.config.panel_width = Some(w),
            Side::Files => self.config.files_width = Some(w),
        }
        cx.notify();
    }

    pub fn end_resize(&mut self, cx: &mut Context<Self>) {
        if self.resizing.take().is_some() {
            self.config.save();
            cx.notify();
        }
    }

    /// Double-click on an edge: back to the default width.
    pub fn reset_width(&mut self, side: Side, cx: &mut Context<Self>) {
        match side {
            Side::Left => self.config.sidebar_width = None,
            Side::Right => self.config.panel_width = None,
            Side::Files => self.config.files_width = None,
        }
        self.config.save();
        cx.notify();
    }

    /// Folds or unfolds a sidebar section; remembered across launches.
    pub fn toggle_section(&mut self, key: &str, cx: &mut Context<Self>) {
        let c = &mut self.config.collapsed;
        match c.iter().position(|k| k == key) {
            Some(i) => {
                c.remove(i);
            }
            None => c.push(key.to_owned()),
        }
        self.config.save();
        cx.notify();
    }

    pub fn toggle_split(&mut self, cx: &mut Context<Self>) {
        self.config.split_diff = !self.config.split_diff;
        self.config.save();
        cx.notify();
    }

    pub fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.config.light = !self.config.light;
        self.config.save();
        theme::apply(self.config.light, Some(window), cx);
        // Syntax colors depend on the theme.
        self.refresh_diff(cx);
        cx.notify();
    }

    /// Folder picker; `init` turns the folder into a new repository first.
    pub fn pick_folder(&mut self, init: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = if init { "Créer le dépôt ici" } else { "Importer" };
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some(prompt.into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update_in(cx, |this, window, cx| {
                if init {
                    let p = path.clone();
                    let job = GitJob::new("Création du dépôt", &["init"]).dir(path).after(move |this, window, cx| this.import_path(p, window, cx));
                    this.run(job, window, cx);
                } else {
                    this.import_path(path, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn pick_clone_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choisir".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(p) = paths.into_iter().next() {
                    this.clone_parent = p;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn import_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match Repository::discover(&path) {
            Ok(r) => {
                let root = repo_root(&r);
                self.page = Page::Repo;
                self.load_project(root, window, cx);
            }
            Err(_) => window.push_notification(
                Notification::warning(format!(
                    "{} n'est pas un dépôt git. Pour en créer un, utilise « Créer un nouveau dépôt ».",
                    path.display()
                ))
                .title("Pas un dépôt git"),
                cx,
            ),
        }
    }

    /// Clones the address typed on the import page.
    pub fn clone_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.clone_url.read(cx).value().trim().to_owned();
        self.clone_from(url, window, cx);
    }

    /// Clones `url` into the chosen folder, then opens it.
    pub fn clone_from(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = repo_name(&url) else {
            return window.push_notification(Notification::warning("Colle l'adresse du dépôt à cloner."), cx);
        };
        let dest = self.clone_parent.join(name);
        // Cloned earlier: open it rather than complain.
        if Repository::open(&dest).ok().and_then(|r| r.find_remote("origin").ok()?.url().map(|u| u == url)).unwrap_or(false) {
            return self.import_path(dest, window, cx);
        }
        if dest.exists() {
            let msg = format!("{} existe déjà : choisis un autre dossier de destination.", dest.display());
            return window.push_notification(Notification::warning(msg), cx);
        }
        let d = dest.clone();
        let job = GitJob::owned("Clonage", clone_args(&url, &dest))
            .network()
            .dir(self.clone_parent.clone())
            .done("Dépôt cloné")
            .after(move |this, window, cx| {
                this.clone_url.update(cx, |s, cx| s.set_value("", window, cx));
                this.import_path(d, window, cx);
            });
        self.run(job, window, cx);
    }
}

/// The last commit can be undone or amended: there is one, on a branch, nothing half-done, and no
/// server has it yet (rewriting it there would need a forced push and break others' copies).
pub fn rewritable(h: &History) -> bool {
    h.has_commits && h.head_branch.is_some() && h.state == git2::RepositoryState::Clean && !h.head_published
}

/// A full load read the file statuses before a status refresh that started after it (e.g. a
/// file was checked right after coming back to the window): keep the refresh's newer lists.
fn keep_fresher_changes(loaded: &mut History, current: Option<&mut Project>, stale: bool) {
    if let (true, Some(p)) = (stale, current) {
        loaded.changes = std::mem::take(&mut p.history.changes);
    }
}

/// Panel widths that fit the window: when the history would get narrower than its minimum,
/// the right panel shrinks first, then the sidebar, never below their own minimum.
/// The widths the user chose stay saved and come back when the window grows again.
pub fn fit_widths(viewport: f32, (mut sidebar, mut panel): (f32, f32)) -> (f32, f32) {
    let over = sidebar + panel + CENTER_MIN_W - viewport;
    if over > 0. {
        let from_panel = over.min(panel - PANEL_RANGE.0).max(0.);
        panel -= from_panel;
        sidebar -= (over - from_panel).min(sidebar - SIDEBAR_RANGE.0).max(0.);
    }
    (sidebar, panel)
}

/// Name and e-mail that sign commits now, from the user's global git config (empty when unset).
fn current_identity() -> (String, String) {
    let get = |key| git::run(&config::home(), &["config", "--global", "--get", key]).unwrap_or_default();
    (get("user.name"), get("user.email"))
}

/// Name and e-mail suggested to sign commits, from the GitHub account. Nothing when the account
/// couldn't be checked (offline start: its login is only a placeholder).
fn identity_suggestion(login: &Login) -> (String, String) {
    match login {
        Login::In(a) if !a.noreply_email.is_empty() => (a.name.clone().unwrap_or_else(|| a.login.clone()), a.noreply_email.clone()),
        _ => (String::new(), String::new()),
    }
}

/// Saves the commit author in the user's global git config (`env` lets tests aim at a temp file).
pub fn save_identity(name: &str, email: &str, env: &[(String, String)]) -> Result<(), String> {
    if name.is_empty() {
        return Err("Indique ton nom.".into());
    }
    if !email.contains('@') || email.contains(char::is_whitespace) {
        return Err("Indique une adresse e-mail valide.".into());
    }
    for (key, value) in [("user.name", name), ("user.email", email)] {
        git::exec(&config::home(), &["config", "--global", key, value], None, None, env)?;
    }
    Ok(())
}

/// Where a new branch gets published, like git decides: `remote.pushDefault`, else `origin`,
/// else the only (or first) remote.
/// Git's ways of saying the server wants credentials it was not given (prompts are disabled).
fn needs_login(git_msg: &str) -> bool {
    [
        "could not read Username",
        "terminal prompts disabled",
        "Authentication failed",
        "Invalid username or password",
        "Permission denied (publickey)",
        "The requested URL returned error: 403",
        "The requested URL returned error: 401",
    ]
    .iter()
    .any(|s| git_msg.contains(s))
}

pub fn push_remote(repo: &Repository) -> String {
    let names: Vec<String> = repo.remotes().map(|r| r.iter().flatten().map(str::to_owned).collect()).unwrap_or_default();
    repo.config()
        .and_then(|c| c.get_string("remote.pushDefault"))
        .ok()
        .or_else(|| names.iter().find(|n| *n == "origin").cloned())
        .or_else(|| names.first().cloned())
        .unwrap_or_else(|| "origin".into())
}

/// `git pull` arguments. Since git 2.33, pulling diverged branches fails until the user says
/// how to reconcile them: merge, unless they chose otherwise with `pull.rebase`.
pub fn pull_args(repo: &Repository) -> Vec<&'static str> {
    let chosen = repo.config().and_then(|c| c.get_string("pull.rebase")).is_ok();
    let mut args = vec!["pull", "--no-edit"];
    if !chosen {
        args.push("--no-rebase");
    }
    args
}

/// Canonical project folder (no trailing slash, symlinks resolved) so a project is listed once.
pub fn repo_root(r: &Repository) -> PathBuf {
    let p = r.workdir().unwrap_or(r.path());
    canonical(p).unwrap_or_else(|| p.to_owned())
}

/// `fs::canonicalize`, minus the `\\?\` prefix it adds on Windows: git, dialogs and users
/// expect `C:\dev\projet`, not `\\?\C:\dev\projet`.
pub fn canonical(p: &std::path::Path) -> Option<PathBuf> {
    let full = std::fs::canonicalize(p).ok()?;
    if cfg!(windows) {
        let plain = full.to_str().and_then(strip_verbatim).map(PathBuf::from);
        return Some(plain.unwrap_or(full));
    }
    Some(full)
}

/// `\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share` → `\\srv\share`; None when there is no prefix.
fn strip_verbatim(s: &str) -> Option<String> {
    let rest = s.strip_prefix(r"\\?\")?;
    Some(match rest.strip_prefix(r"UNC\") {
        Some(unc) => format!(r"\\{unc}"),
        None => rest.to_owned(),
    })
}

/// Checkout of a branch. A remote one (`upstream/feature`) goes to the local branch of that name
/// if there is one, else creates it tracking exactly that remote: a plain `git switch feature`
/// fails as soon as two remotes have a `feature`.
pub fn checkout_args(name: &str, remote: bool, locals: &[&str]) -> Vec<String> {
    let local = name.split_once('/').map_or(name, |(_, b)| b);
    match remote && !locals.contains(&local) {
        true => vec!["switch".into(), "--track".into(), name.into()],
        false => vec!["switch".into(), if remote { local } else { name }.into()],
    }
}

/// `--` first: whatever was pasted is taken as an address, never as a git option.
pub fn clone_args(url: &str, dest: &std::path::Path) -> Vec<String> {
    vec!["clone".into(), "--".into(), url.into(), dest.display().to_string()]
}

/// Folder name `git clone` would pick: `https://github.com/a/b.git` and `git@github.com:a/b` give `b`.
pub fn repo_name(url: &str) -> Option<&str> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    url.rsplit(['/', ':']).next().filter(|s| !s.is_empty() && *s != url)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{GitJob, needs_login};
    use std::path::Path;

    /// Temp folder unique to this test run.
    pub fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rameau-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// git with a throwaway identity, panicking with git's message on failure.
    pub fn git(dir: &Path, args: &[&str]) -> String {
        let mut a = vec!["-c", "user.name=t", "-c", "user.email=t@t"];
        a.extend(args);
        crate::git::run(dir, &a).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    #[test]
    fn pull_merges_diverged_branches() {
        let root = temp("pull");
        git(&root, &["init", "-q", "--bare", "remote.git"]);
        git(&root, &["clone", "-q", "remote.git", "a"]);
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::write(a.join("f"), "1\n").unwrap();
        git(&a, &["add", "f"]);
        git(&a, &["commit", "-qm", "base"]);
        git(&a, &["push", "-q", "origin", "HEAD"]);
        git(&root, &["clone", "-q", "remote.git", "b"]);
        // Diverge: one commit pushed from b, another local to a.
        std::fs::write(b.join("g"), "b\n").unwrap();
        git(&b, &["add", "g"]);
        git(&b, &["commit", "-qm", "from b"]);
        git(&b, &["push", "-q"]);
        std::fs::write(a.join("h"), "a\n").unwrap();
        git(&a, &["add", "h"]);
        git(&a, &["commit", "-qm", "from a"]);

        let repo = git2::Repository::open(&a).unwrap();
        let args = super::pull_args(&repo);
        // Git for Windows sets pull.rebase at install: then the user's choice applies.
        let preset = repo.config().unwrap().get_string("pull.rebase").is_ok();
        assert_eq!(args.contains(&"--no-rebase"), !preset);
        git(&a, &args);
        assert!(a.join("g").exists() && a.join("h").exists());

        // An explicit choice is left to git.
        git(&a, &["config", "pull.rebase", "true"]);
        let repo = git2::Repository::open(&a).unwrap();
        assert!(!super::pull_args(&repo).contains(&"--no-rebase"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_branch_goes_to_the_right_remote() {
        let dir = temp("push");
        git(&dir, &["init", "-q"]);
        let open = || git2::Repository::open(&dir).unwrap();
        git(&dir, &["remote", "add", "upstream", "https://example.com/u.git"]);
        assert_eq!(super::push_remote(&open()), "upstream", "single remote");
        // "fork" sorts before "origin": origin must still win.
        git(&dir, &["remote", "add", "fork", "https://example.com/f.git"]);
        git(&dir, &["remote", "add", "origin", "https://example.com/o.git"]);
        assert_eq!(super::push_remote(&open()), "origin");
        git(&dir, &["config", "remote.pushDefault", "fork"]);
        assert_eq!(super::push_remote(&open()), "fork");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panels_fit_a_shrinking_window() {
        // Wide panels chosen on a big screen, then the window goes down to its 900px minimum.
        assert_eq!(super::fit_widths(900., (440., 680.)), (200., 300.));
        // Small overflow: only the right panel gives way.
        assert_eq!(super::fit_widths(1320., (256., 680.)), (256., 644.));
        // Enough room: untouched.
        assert_eq!(super::fit_widths(1600., (256., 380.)), (256., 380.));
    }

    #[test]
    fn late_full_load_keeps_newer_file_status() {
        let dir = temp("race");
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("f"), "1\n").unwrap();
        let open = || git2::Repository::open(&dir).unwrap();
        // The full load reads the status first: f is not staged yet...
        let mut loaded = crate::repo::load(&mut open(), 0).unwrap();
        assert_eq!(loaded.changes.staged.len(), 0);
        // ...then f gets checked and the status refresh lands before the load.
        git(&dir, &["add", "f"]);
        let mut current = super::Project { path: dir.clone(), repo: open(), history: crate::repo::load(&mut open(), 0).unwrap() };
        assert_eq!(current.history.changes.staged.len(), 1);
        super::keep_fresher_changes(&mut loaded, Some(&mut current), true);
        assert_eq!(loaded.changes.staged.len(), 1, "the late load must not uncheck f");
        // Not stale: the load's own result is used.
        let mut fresh = crate::repo::load(&mut open(), 0).unwrap();
        fresh.changes.staged.clear();
        super::keep_fresher_changes(&mut fresh, Some(&mut current), false);
        assert!(fresh.changes.staged.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_paths_lose_their_verbatim_prefix() {
        assert_eq!(super::strip_verbatim(r"\\?\C:\dev\projet").as_deref(), Some(r"C:\dev\projet"));
        assert_eq!(super::strip_verbatim(r"\\?\UNC\nas\code\p").as_deref(), Some(r"\\nas\code\p"));
        assert_eq!(super::strip_verbatim(r"C:\dev"), None);
        assert_eq!(super::strip_verbatim("/Users/x"), None);
    }

    #[test]
    fn offline_account_suggests_no_identity() {
        use super::{Login, identity_suggestion};
        use crate::github::Account;
        let account = |name: Option<&str>, email: &str| {
            Login::In(Account { login: "ada".into(), token: "t".into(), name: name.map(Into::into), noreply_email: email.into(), avatar_url: None })
        };
        let checked = account(Some("Ada Lovelace"), "1+ada@users.noreply.github.com");
        assert_eq!(identity_suggestion(&checked), ("Ada Lovelace".into(), "1+ada@users.noreply.github.com".into()));
        // Started offline: the account was kept, but nothing about it is known yet.
        let offline = Login::In(Account { login: "GitHub".into(), token: "t".into(), name: None, noreply_email: String::new(), avatar_url: None });
        assert_eq!(identity_suggestion(&offline), (String::new(), String::new()));
        assert_eq!(identity_suggestion(&Login::Out), (String::new(), String::new()));
    }

    #[test]
    fn identity_is_checked_and_saved() {
        let dir = temp("identity");
        let file = dir.join("gitconfig");
        let env = [("GIT_CONFIG_GLOBAL".to_owned(), file.display().to_string())];
        assert!(super::save_identity("", "a@b.c", &env).is_err());
        assert!(super::save_identity("Ada", "pas-une-adresse", &env).is_err());
        super::save_identity("Ada Lovelace", "ada@example.com", &env).unwrap();
        let get = |key: &str| crate::git::exec(&dir, &["config", "--global", key], None, None, &env).unwrap();
        assert_eq!((get("user.name"), get("user.email")), ("Ada Lovelace".into(), "ada@example.com".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hooks_see_the_token_only_when_given() {
        let dir = temp("hook-token");
        git(&dir, &["init", "-q"]);
        // A pre-commit hook writing out whatever token it can see.
        let leak = dir.join("leak");
        let hook = dir.join(".git/hooks/pre-commit");
        std::fs::write(&hook, format!("#!/bin/sh\nprintf '%s' \"$RAMEAU_TOKEN\" > '{}'\n", leak.display())).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        // Without the machine's git config: a github.com login stored there (GitHub Desktop, gh…)
        // rightly keeps the token away from git, which is not what this test is about.
        let global = dir.join("gitconfig-vide");
        std::fs::write(&global, "").unwrap();
        let env = [("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()), ("GIT_CONFIG_GLOBAL".to_owned(), global.display().to_string())];
        let commit = |token| {
            let args = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"];
            crate::git::exec(&dir, &args, None, token, &env).unwrap();
            std::fs::read_to_string(&leak).unwrap()
        };
        // What run() passes to a commit: no token, nothing to read.
        assert!(!GitJob::new("Commit", &["commit"]).network);
        assert_eq!(commit(None), "");
        // Why: given a token, any hook reads it.
        assert_eq!(commit(Some("s3cret")), "s3cret");
        assert!(GitJob::new("Push", &["push"]).network().network);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refused_credentials_are_recognized() {
        // HTTPS without any login (prompts disabled), with a wrong one, and SSH without a key.
        assert!(needs_login("fatal: could not read Username for 'https://github.com': terminal prompts disabled"));
        assert!(needs_login("remote: Invalid username or password.\nfatal: Authentication failed for 'https://github.com/a/b.git/'"));
        assert!(needs_login("git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository."));
        // Other failures keep their own message.
        assert!(!needs_login("! [rejected]        main -> main (fetch first)"));
        assert!(!needs_login("fatal: unable to access 'https://github.com/a/b.git/': Could not resolve host: github.com"));
    }

    #[test]
    fn pasted_options_are_not_run() {
        let dir = temp("clone-opt");
        let marker = dir.join("ran");
        // A pasted "address" that is really an option running a command.
        let url = format!("--upload-pack=touch '{}'", marker.display());
        git(&dir, &["init", "-q", "--bare", "src.git"]);
        let args = super::clone_args(&url, &dir.join("src.git"));
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        assert!(crate::git::run(&dir, &a).is_err(), "taken as an address, it doesn't exist");
        assert!(!marker.exists(), "the pasted command must not run");
        // A normal address still clones.
        let src = dir.join("src.git").display().to_string();
        let args = super::clone_args(&src, &dir.join("copie"));
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        crate::git::run(&dir, &a).unwrap();
        assert!(dir.join("copie/.git").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkout_of_a_branch_present_on_two_remotes() {
        let root = temp("two-remotes");
        git(&root, &["init", "-q", "-b", "main", "up"]);
        let up = root.join("up");
        git(&up, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&up, &["branch", "feature"]);
        git(&root, &["clone", "-q", "up", "work"]);
        let work = root.join("work");
        git(&work, &["remote", "add", "upstream", "../up"]);
        git(&work, &["fetch", "-q", "upstream"]);
        let args = super::checkout_args("upstream/feature", true, &["main"]);
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        git(&work, &a);
        assert_eq!(git(&work, &["rev-parse", "--abbrev-ref", "@{upstream}"]), "upstream/feature");
        // Once the local branch exists, it is simply switched to.
        assert_eq!(super::checkout_args("origin/feature", true, &["main", "feature"]), ["switch", "feature"]);
        assert_eq!(super::checkout_args("main", false, &["main"]), ["switch", "main"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn repo_name() {
        assert_eq!(super::repo_name("https://github.com/a/b.git"), Some("b"));
        assert_eq!(super::repo_name("git@github.com:a/rameau.git"), Some("rameau"));
        assert_eq!(super::repo_name("https://gitlab.com/g/sub/proj/"), Some("proj"));
        assert_eq!(super::repo_name("   "), None);
        assert_eq!(super::repo_name("pasuneurl"), None);
    }
}
