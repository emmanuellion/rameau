// Windows release: GUI app, no console window alongside.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod config;
mod diff_view;
mod git;
mod github;
mod graph;
mod import;
mod patch;
mod rebase;
mod repo;
mod theme;
mod ui;
mod view;

use git2::Repository;
use gpui::{App, AppContext, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, point, px, size};
use gpui_component::{Root, TitleBar};

use app::{CloseDiff, Rameau, SelectNext, SelectPrev};
use config::Config;

fn main() {
    let mut config = Config::load();
    // `rameau <path>` opens (and imports) that project.
    let open = std::env::args().nth(1).map(|arg| match Repository::discover(&arg) {
        Ok(r) => app::repo_root(&r),
        Err(e) => {
            eprintln!("rameau: pas de dépôt git à {arg} : {}", e.message());
            std::process::exit(1);
        }
    });
    // Forget projects deleted from disk since last time, list each folder once.
    let mut projects: Vec<_> = config.projects.iter().filter_map(|p| std::fs::canonicalize(p).ok()).collect();
    projects.dedup();
    config.projects = projects;
    config.last = config.last.as_ref().and_then(|p| std::fs::canonicalize(p).ok()).or_else(|| config.projects.first().cloned());

    Application::new().with_assets(ui::Assets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        theme::apply(config.light, None, cx);
        cx.bind_keys([
            KeyBinding::new("up", SelectPrev, Some("Rameau")),
            KeyBinding::new("down", SelectNext, Some("Rameau")),
            KeyBinding::new("escape", CloseDiff, Some("Rameau")),
        ]);
        let bounds = Bounds::centered(None, size(px(1320.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // Traffic lights vertically centered in our taller title bar.
                titlebar: Some(TitlebarOptions {
                    traffic_light_position: Some(point(px(14.), px((view::TITLE_BAR_H - 14.) / 2.))),
                    ..TitleBar::title_bar_options()
                }),
                // Linux: draw our own title bar and window buttons (Wayland compositors like GNOME don't).
                window_decorations: cfg!(target_os = "linux").then_some(WindowDecorations::Client),
                window_min_size: Some(size(px(900.), px(560.))),
                app_id: Some("rameau".into()),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Rameau");
                let view = cx.new(|cx| Rameau::new(config, open, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("open window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
