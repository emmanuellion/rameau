//! Application state and actions. Rendering lives in `view.rs`, `diff_view.rs` and `import.rs`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use git2::{Oid, Repository};
use gpui::{
    ClipboardItem, Context, Entity, FocusHandle, HighlightStyle, PathPromptOptions, ScrollStrategy, SharedString,
    Subscription, Task, UniformListScrollHandle, Window, actions, div, prelude::*, px,
};
use gpui_component::{
    WindowExt,
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
    Changes,
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
}

/// Which panel edge is being dragged.
#[derive(Clone, Copy, PartialEq)]
pub enum Side {
    Left,
    Right,
}

const SIDEBAR_W: f32 = 256.;
const PANEL_W: f32 = 380.;
const SIDEBAR_RANGE: (f32, f32) = (200., 440.);
const PANEL_RANGE: (f32, f32) = (300., 680.);
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
    pub selection: Selection,
    pub details: Option<Details>,
    pub diff: Option<OpenDiff>,
    pub scroll: UniformListScrollHandle,
    pub diff_scroll: UniformListScrollHandle,
    pub message: Entity<InputState>,
    pub clone_url: Entity<InputState>,
    pub clone_parent: PathBuf,
    pub busy: Option<SharedString>,
    pub login: Login,
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
        let message = cx.new(|cx| InputState::new(window, cx).multi_line(true).placeholder("Message du commit"));
        let clone_url = cx.new(|cx| InputState::new(window, cx).placeholder("https://github.com/utilisateur/projet.git"));
        let subscriptions = vec![
            // Pick up changes made outside the app (editor, terminal) when coming back to it.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh_on_focus(window, cx);
                }
            }),
            cx.subscribe_in(&message, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { secondary: true } = ev {
                    this.commit(window, cx);
                }
            }),
        ];
        cx.spawn(async move |this, cx| {
            let account = cx.background_executor().spawn(async { github::load() }).await;
            this.update(cx, |this, cx| {
                if let Some(a) = account {
                    this.login = Login::In(a);
                    cx.notify();
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
            selection: Selection::None,
            details: None,
            diff: None,
            scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
            message,
            clone_url,
            clone_parent,
            busy: None,
            login: Login::Out,
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
        let task = cx.background_executor().spawn(async move {
            let mut repo = Repository::open(&p)?;
            let history = repo::load(&mut repo)?;
            Ok::<_, git2::Error>((repo, history))
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
                            window.set_window_title(&format!("{} — Rameau", history.name));
                            this.config.add(&path);
                            this.config.save();
                            this.page = Page::Repo;
                        }
                        this.proj = Some(Project { path, repo, history });
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
            this.update_in(cx, |this, window, cx| if moved { this.reload(window, cx) } else { this.refresh_status(cx) }).ok();
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

    pub fn select_changes(&mut self, cx: &mut Context<Self>) {
        self.selection = Selection::Changes;
        self.close_diff();
        cx.notify();
    }

    /// ↑/↓: walk the history (the "local changes" row sits above the first commit).
    pub fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(h) = self.history() else { return };
        let wip = !h.changes.is_empty();
        let current = match self.selection {
            Selection::Changes => Some(-1),
            Selection::Commit(id) => h.commits.iter().position(|c| c.id == id).map(|i| i as isize),
            Selection::None => None,
        };
        let next = current.map_or(if wip { -1 } else { 0 }, |i| i + delta);
        if next == -1 && wip {
            self.select_changes(cx);
        } else if next >= 0 && (next as usize) < h.commits.len() {
            let id = h.commits[next as usize].id;
            self.scroll.scroll_to_item(next as usize, ScrollStrategy::Top);
            self.select_commit(id, cx);
        }
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
            Ok::<_, git2::Error>(OpenDiff {
                source,
                path,
                lines,
                selectable,
                highlights,
                selected: BTreeSet::new(),
                anchor: None,
                longest,
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
    /// Also closes the changes panel once nothing is left to commit.
    pub fn refresh_diff(&mut self, cx: &mut Context<Self>) {
        if self.selection == Selection::Changes && self.history().is_some_and(|h| h.changes.is_empty()) {
            self.selection = Selection::None;
        }
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
        let GitJob { what, args, input, env, done, after, status_only, .. } = job;
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
        if h.changes.staged.is_empty() {
            window.push_notification(Notification::warning("Coche au moins un fichier à inclure dans le commit."), cx);
            return;
        }
        let msg = self.message.read(cx).value().trim().to_owned();
        if msg.is_empty() {
            window.push_notification(Notification::warning("Écris un message de commit."), cx);
            self.message.update(cx, |s, cx| s.focus(window, cx));
            return;
        }
        // A fresh Git (Windows, Linux) has no author: git would refuse the commit in English.
        let signed = self.proj.as_ref().is_some_and(|p| Repository::open(p.repo.path()).and_then(|r| r.signature()).is_ok());
        if !signed {
            return self.identity_dialog(window, cx);
        }
        let job = GitJob::owned("Commit", vec!["commit".into(), "-m".into(), msg])
            .after(|this, window, cx| this.message.update(cx, |s, cx| s.set_value("", window, cx)));
        self.run(job, window, cx);
    }

    /// Asks the name and e-mail that sign commits, saves them in the user's git config, then commits.
    fn identity_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name0, email0) = identity_suggestion(&self.login);
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
                .child(Input::new(&email));
            ui::with_footer(dialog.title("Qui signe les commits ?").child(body), "Enregistrer et committer", false, move |window, cx| {
                let (n, e) = (name.read(cx).value().trim().to_owned(), email.read(cx).value().trim().to_owned());
                if let Err(msg) = save_identity(&n, &e, &[]) {
                    window.push_notification(Notification::warning(msg), cx);
                    return false;
                }
                weak.update(cx, |this, cx| this.commit(window, cx)).ok();
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
                        this.login = Login::In(a);
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

    /// Follows the mouse while a panel edge is dragged (`x` from the window's left edge).
    pub fn resize(&mut self, side: Side, x: f32, window_w: f32, cx: &mut Context<Self>) {
        let (w, min, max) = match side {
            Side::Left => (x, SIDEBAR_RANGE.0, SIDEBAR_RANGE.1.min(window_w - self.panel_w() - CENTER_MIN_W)),
            Side::Right => (window_w - x, PANEL_RANGE.0, PANEL_RANGE.1.min(window_w - self.sidebar_w() - CENTER_MIN_W)),
        };
        let w = w.clamp(min, max.max(min)).round();
        match side {
            Side::Left => self.config.sidebar_width = Some(w),
            Side::Right => self.config.panel_width = Some(w),
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

    pub fn clone_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.clone_url.read(cx).value().trim().to_owned();
        let Some(name) = repo_name(&url) else {
            return window.push_notification(Notification::warning("Colle l'adresse du dépôt à cloner."), cx);
        };
        let dest = self.clone_parent.join(name);
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
    use super::GitJob;
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
        let mut loaded = crate::repo::load(&mut open()).unwrap();
        assert_eq!(loaded.changes.staged.len(), 0);
        // ...then f gets checked and the status refresh lands before the load.
        git(&dir, &["add", "f"]);
        let mut current = super::Project { path: dir.clone(), repo: open(), history: crate::repo::load(&mut open()).unwrap() };
        assert_eq!(current.history.changes.staged.len(), 1);
        super::keep_fresher_changes(&mut loaded, Some(&mut current), true);
        assert_eq!(loaded.changes.staged.len(), 1, "the late load must not uncheck f");
        // Not stale: the load's own result is used.
        let mut fresh = crate::repo::load(&mut open()).unwrap();
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
            Login::In(Account { login: "ada".into(), token: "t".into(), name: name.map(Into::into), noreply_email: email.into() })
        };
        let checked = account(Some("Ada Lovelace"), "1+ada@users.noreply.github.com");
        assert_eq!(identity_suggestion(&checked), ("Ada Lovelace".into(), "1+ada@users.noreply.github.com".into()));
        // Started offline: the account was kept, but nothing about it is known yet.
        let offline = Login::In(Account { login: "GitHub".into(), token: "t".into(), name: None, noreply_email: String::new() });
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
        let commit = |token| {
            let args = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"];
            crate::git::exec(&dir, &args, None, token, &[]).unwrap();
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
