//! Application state and actions. Rendering lives in `view.rs`, `diff_view.rs` and `import.rs`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use git2::{Oid, Repository};
use gpui::{
    ClipboardItem, Context, Entity, FocusHandle, HighlightStyle, PathPromptOptions, ScrollStrategy, SharedString,
    Subscription, Task, UniformListScrollHandle, Window, actions, prelude::*, px,
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
/// The history keeps at least this much room when panels grow.
const CENTER_MIN_W: f32 = 420.;

#[derive(Clone, Copy, PartialEq)]
pub enum LineOp {
    Stage,
    Unstage,
    Discard,
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
}

impl GitJob {
    pub fn new(what: &'static str, args: &[&str]) -> Self {
        Self::owned(what, args.iter().map(|s| s.to_string()).collect())
    }

    pub fn owned(what: &'static str, args: Vec<String>) -> Self {
        Self { what, args, dir: None, input: None, env: vec![], done: None, after: None }
    }

    pub fn done(mut self, msg: &'static str) -> Self {
        self.done = Some(msg);
        self
    }

    pub fn after(mut self, f: impl FnOnce(&mut Rameau, &mut Window, &mut Context<Rameau>) + 'static) -> Self {
        self.after = Some(Box::new(f));
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
    load_task: Option<Task<()>>,
    login_task: Option<Task<()>>,
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
                    this.reload(window, cx);
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
            load_task: None,
            login_task: None,
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
                    Ok((repo, history)) => {
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
                            this.diff = None;
                            this.scroll = UniformListScrollHandle::new();
                            window.set_window_title(&format!("{} — Rameau", history.name));
                            this.config.add(&path);
                            this.config.save();
                            this.page = Page::Repo;
                        }
                        this.proj = Some(Project { path, repo, history });
                        if this.selection == Selection::Changes && this.history().is_some_and(|h| h.changes.is_empty()) {
                            this.selection = Selection::None;
                        }
                        this.refresh_diff();
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

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.pending.clone().or_else(|| self.dir()) {
            self.load_project(path, window, cx);
        }
    }

    pub fn remove_current_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.proj.take() else { return };
        self.config.remove(&p.path);
        self.config.save();
        self.diff = None;
        self.selection = Selection::None;
        match self.config.last.clone() {
            Some(next) => self.load_project(next, window, cx),
            None => self.page = Page::Import,
        }
        cx.notify();
    }

    pub fn select_commit(&mut self, id: Oid, cx: &mut Context<Self>) {
        let Some(p) = &self.proj else { return };
        self.selection = Selection::Commit(id);
        self.diff = None;
        self.details = repo::details(&p.repo, id).inspect_err(|e| eprintln!("rameau: {e}")).ok();
        cx.notify();
    }

    pub fn select_changes(&mut self, cx: &mut Context<Self>) {
        self.selection = Selection::Changes;
        self.diff = None;
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

    pub fn open_diff(&mut self, source: DiffSource, path: String) {
        let Some(p) = &self.proj else { return };
        match repo::file_diff(&p.repo, &source, &path) {
            Ok((lines, partial)) => {
                let highlights = diff_view::highlight(&p.repo, &source, &path, &lines);
                let longest = lines.iter().enumerate().max_by_key(|(_, l)| l.text.len()).map_or(0, |(i, _)| i);
                let selectable = partial && !matches!(source, DiffSource::Commit(_));
                self.diff = Some(OpenDiff {
                    source,
                    path,
                    lines,
                    selectable,
                    highlights,
                    selected: BTreeSet::new(),
                    anchor: None,
                    longest,
                });
                self.diff_scroll = UniformListScrollHandle::new();
            }
            Err(e) => eprintln!("rameau: {}", e.message()),
        }
    }

    /// After a reload, keep the open working-copy diff in sync: it follows the file
    /// from "unstaged" to "staged" (and back), and closes when the file is clean.
    pub fn refresh_diff(&mut self) {
        let Some(d) = self.diff.take() else { return };
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
        if let Some(source) = [d.source.clone(), other].into_iter().find(has) {
            let scroll = self.diff_scroll.clone();
            self.open_diff(source, d.path);
            self.diff_scroll = scroll;
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
        let token = self.token();
        let GitJob { what, args, input, env, done, after, .. } = job;
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
                this.reload(window, cx);
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
        let job = GitJob::owned("Commit", vec!["commit".into(), "-m".into(), msg])
            .after(|this, window, cx| this.message.update(cx, |s, cx| s.set_value("", window, cx)));
        self.run(job, window, cx);
    }

    pub fn push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.proj else { return };
        let job = if p.history.ahead_behind.is_some() {
            GitJob::new("Push", &["push"])
        } else {
            // First push of this branch: publish it and set its upstream.
            let remote = p.repo.remotes().ok().and_then(|r| r.get(0).map(str::to_owned)).unwrap_or_else(|| "origin".into());
            GitJob::new("Push", &["push", "-u", &remote, "HEAD"])
        };
        self.run(job.done("Push terminé"), window, cx);
    }

    /// Stages, unstages or discards the selected lines of the open diff (or one hunk).
    pub fn apply_lines(&mut self, op: LineOp, hunk: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = &self.diff else { return };
        let hunk_end = hunk.map(|h| (h + 1..d.lines.len()).find(|&i| d.lines[i].kind == LineKind::Hunk).unwrap_or(d.lines.len()));
        let selected = |i: usize| match (hunk, hunk_end) {
            (Some(h), Some(end)) => i > h && i < end,
            _ => d.selected.contains(&i),
        };
        let Some(patch) = patch::build(&d.path, &d.lines, selected, op == LineOp::Unstage) else { return };
        let (what, args): (&'static str, &[&str]) = match op {
            LineOp::Stage => ("Ajout des lignes", &["apply", "--cached", "--recount", "-"]),
            LineOp::Unstage => ("Retrait des lignes", &["apply", "--cached", "--recount", "--reverse", "-"]),
            LineOp::Discard => ("Abandon des lignes", &["apply", "--recount", "--reverse", "-"]),
        };
        let job = GitJob::new(what, args).input(patch);
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
            (GitJob::owned("Suppression", vec!["clean".into(), "-f".into(), "--".into(), path.clone()]), format!("Le nouveau fichier {path} sera supprimé."))
        } else {
            (
                GitJob::owned("Annulation", vec!["restore".into(), "--".into(), path.clone()]),
                format!("Les modifications non indexées de {path} seront perdues définitivement."),
            )
        };
        self.confirm("Abandonner les modifications ?".into(), msg, "Abandonner les modifications", job, window, cx);
    }

    pub fn delete_branch(&mut self, name: String, kind: RefKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.proj else { return };
        if kind == RefKind::Remote {
            let Some((remote, branch)) = name.split_once('/') else { return };
            let job = GitJob::new("Suppression de la branche distante", &["push", remote, "--delete", branch]);
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
        window.open_dialog(cx, move |dialog, _, _| {
            let (editor, weak) = (editor.clone(), weak.clone());
            let dialog = dialog.title("Rebase interactif").w(px(760.)).child(editor.clone());
            ui::with_footer(dialog, "Lancer le rebase", false, move |window, cx| match editor.read(cx).plan(&std::env::temp_dir(), cx) {
                Err(e) => {
                    window.push_notification(Notification::error(e), cx);
                    false
                }
                Ok((base, todo)) => {
                    weak.update(cx, |this, cx| this.run_rebase(base, todo, window, cx)).ok();
                    true
                }
            })
        });
    }

    fn run_rebase(&mut self, base: Option<Oid>, todo: String, window: &mut Window, cx: &mut Context<Self>) {
        let file = std::env::temp_dir().join(format!("rameau-todo-{}.txt", std::process::id()));
        if let Err(e) = std::fs::write(&file, todo) {
            return self.notify_error("Rebase interactif", e.to_string(), window, cx);
        }
        let mut args = vec!["rebase".to_owned(), "-i".into(), "--autostash".into()];
        args.push(base.map_or("--root".into(), |b| b.to_string()));
        let mut job = GitJob::owned("Rebase interactif", args).done("Rebase terminé");
        // git calls the "editor" with the todo path: we just copy our list over it.
        job.env.push(("GIT_SEQUENCE_EDITOR".into(), format!("cp '{}'", file.display())));
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
            let result = cx
                .background_executor()
                .spawn(async move {
                    let account = github::wait_for_token(client_id, &code)?;
                    // Not fatal: the login then just lasts for this session.
                    if let Err(e) = github::save(&account) {
                        eprintln!("rameau: token non enregistré : {e}");
                    }
                    Ok::<_, String>(account)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(a) => {
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
        if let Login::In(a) = &self.login {
            github::forget(a);
        }
        self.login = Login::Out;
        self.login_task = None;
        cx.notify();
    }

    pub fn sidebar_w(&self) -> f32 {
        self.config.sidebar_width.unwrap_or(SIDEBAR_W)
    }

    pub fn panel_w(&self) -> f32 {
        self.config.panel_width.unwrap_or(PANEL_W)
    }

    /// Follows the mouse while a panel edge is dragged (`x` from the window's left edge).
    pub fn resize(&mut self, side: Side, x: f32, window_w: f32, cx: &mut Context<Self>) {
        let (w, min, max) = match side {
            Side::Left => (x, 200., 440_f32.min(window_w - self.panel_w() - CENTER_MIN_W)),
            Side::Right => (window_w - x, 300., 680_f32.min(window_w - self.sidebar_w() - CENTER_MIN_W)),
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
        self.refresh_diff();
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
        let job = GitJob::owned("Clonage", vec!["clone".into(), url, dest.display().to_string()])
            .dir(self.clone_parent.clone())
            .done("Dépôt cloné")
            .after(move |this, window, cx| {
                this.clone_url.update(cx, |s, cx| s.set_value("", window, cx));
                this.import_path(d, window, cx);
            });
        self.run(job, window, cx);
    }
}

/// Canonical project folder (no trailing slash, symlinks resolved) so a project is listed once.
pub fn repo_root(r: &Repository) -> PathBuf {
    let p = r.workdir().unwrap_or(r.path());
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned())
}

/// Folder name `git clone` would pick: `https://github.com/a/b.git` and `git@github.com:a/b` give `b`.
pub fn repo_name(url: &str) -> Option<&str> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    url.rsplit(['/', ':']).next().filter(|s| !s.is_empty() && *s != url)
}

#[cfg(test)]
mod tests {
    #[test]
    fn repo_name() {
        assert_eq!(super::repo_name("https://github.com/a/b.git"), Some("b"));
        assert_eq!(super::repo_name("git@github.com:a/rameau.git"), Some("rameau"));
        assert_eq!(super::repo_name("https://gitlab.com/g/sub/proj/"), Some("proj"));
        assert_eq!(super::repo_name("   "), None);
        assert_eq!(super::repo_name("pasuneurl"), None);
    }
}
