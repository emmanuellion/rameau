// Windows release: GUI app, no console window alongside.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod config;
mod conflict;
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
mod update;
mod view;

use git2::Repository;
use gpui::{App, AppContext, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, point, px, size};
use gpui_component::{Root, TitleBar};

use app::{CloseDiff, Fetch, FocusSearch, Pull, Push, Rameau, Refresh, SelectNext, SelectPrev, ShowChanges, ShowHistory};
use config::Config;

fn main() {
    // Before anything else: the Windows setup runs the exe to install or uninstall it.
    update::startup();
    git::load_shell_path();
    let mut config = Config::load();
    // `rameau <path>` opens (and imports) that project.
    let open = std::env::args().nth(1).map(|arg| match Repository::discover(&arg) {
        Ok(r) => app::repo_root(&r),
        Err(e) => {
            eprintln!("rameau: pas de dépôt git à {arg} : {}", e.message());
            std::process::exit(1);
        }
    });
    config.tidy();

    Application::new().with_assets(ui::Assets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        theme::apply(config.light, None, cx);
        cx.bind_keys([
            KeyBinding::new("up", SelectPrev, Some("Rameau")),
            KeyBinding::new("down", SelectNext, Some("Rameau")),
            KeyBinding::new("escape", CloseDiff, Some("Rameau")),
            // "secondary": ⌘ on macOS, Ctrl elsewhere. Shown in the tooltips (ui::shortcut).
            KeyBinding::new("secondary-1", ShowChanges, Some("Rameau")),
            KeyBinding::new("secondary-2", ShowHistory, Some("Rameau")),
            KeyBinding::new("secondary-f", FocusSearch, Some("Rameau")),
            KeyBinding::new("secondary-shift-f", Fetch, Some("Rameau")),
            KeyBinding::new("secondary-shift-p", Pull, Some("Rameau")),
            KeyBinding::new("secondary-p", Push, Some("Rameau")),
            KeyBinding::new("secondary-r", Refresh, Some("Rameau")),
            KeyBinding::new("f5", Refresh, Some("Rameau")),
        ]);
        // Never larger than the screen: at 150 % scaling, 1320×820 is taller than a 1080p display
        // and the bottom of the window (import page included) would be out of reach.
        let wanted = size(px(1320.), px(820.));
        let fit = cx.primary_display().map_or(wanted, |d| {
            let screen = d.bounds().size;
            wanted.min(&size(screen.width * 0.9, screen.height * 0.85))
        });
        let bounds = Bounds::centered(None, fit, cx);
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
