mod format;
mod model;
mod settings;
mod storage_view;
mod sunburst;
mod theme;

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, actions, px, size,
};
use gpui_platform::application;
use tracing_subscriber::prelude::*;

use crate::settings::Settings;
use crate::storage_view::{
    Dismiss, GoToTop, GoUp, OpenSelected, Rescan, RevealInFinder, ScanFolder, ScanHomeFolder,
    ScanStartupDisk, Scope, SelectNext, SelectPrevious, StopScan, StorageView, ToggleCrashReports,
};

pub const APP_ID: &str = "mac-storage-cleaner";
const APP_NAME: &str = "Mac Storage Cleaner";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

actions!(app, [Quit]);

fn main() {
    let settings = Settings::load();
    let _telemetry = telemetry::init(telemetry::Config {
        dsn: option_env!("SENTRY_DSN"),
        release: format!("{APP_ID}@{VERSION}"),
        enabled: settings.crash_reports,
    });
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .with(telemetry::tracing_layer())
        .init();

    if std::env::args().any(|arg| arg == "--test-panic") {
        panic!("test panic from /Users/test-user/Documents/Secret Project/plan.key");
    }
    let scope = std::env::args()
        .skip(1)
        .find(|arg| !arg.starts_with('-'))
        .map_or(Scope::StartupDisk, |path| {
            Scope::Folder(PathBuf::from(path))
        });

    application().run(move |cx: &mut App| {
        cx.on_action(quit);
        cx.bind_keys(key_bindings());
        cx.set_menus(menus(settings.crash_reports));
        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                quit(&Quit, cx);
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1180.), px(780.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(820.), px(560.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(APP_NAME.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| StorageView::new(settings.clone(), scope.clone(), cx));
                let focus = view.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
                view
            },
        );
        if let Err(error) = opened {
            tracing::error!("failed to open main window: {error:#}");
            quit(&Quit, cx);
            return;
        }
        cx.activate(true);
    });
}

fn key_bindings() -> Vec<KeyBinding> {
    let view = Some("StorageView");
    vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("down", SelectNext, view),
        KeyBinding::new("up", SelectPrevious, view),
        KeyBinding::new("enter", OpenSelected, view),
        KeyBinding::new("right", OpenSelected, view),
        KeyBinding::new("cmd-down", OpenSelected, view),
        KeyBinding::new("left", GoUp, view),
        KeyBinding::new("backspace", GoUp, view),
        KeyBinding::new("cmd-up", GoUp, view),
        KeyBinding::new("cmd-shift-up", GoToTop, view),
        KeyBinding::new("escape", Dismiss, view),
        KeyBinding::new("alt-cmd-r", RevealInFinder, view),
        KeyBinding::new("cmd-r", Rescan, view),
        KeyBinding::new("cmd-.", StopScan, view),
        KeyBinding::new("cmd-1", ScanStartupDisk, view),
        KeyBinding::new("cmd-2", ScanHomeFolder, view),
        KeyBinding::new("cmd-o", ScanFolder, view),
    ]
}

pub fn menus(crash_reports: bool) -> Vec<Menu> {
    vec![
        Menu::new(APP_NAME).items([
            MenuItem::action("Send Crash Reports", ToggleCrashReports).checked(crash_reports),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
        ]),
        Menu::new("Scan").items([
            MenuItem::action("Startup Disk", ScanStartupDisk),
            MenuItem::action("Home Folder", ScanHomeFolder),
            MenuItem::action("Folder…", ScanFolder),
            MenuItem::separator(),
            MenuItem::action("Rescan", Rescan),
            MenuItem::action("Stop Scan", StopScan),
        ]),
        Menu::new("Go").items([
            MenuItem::action("Open Folder", OpenSelected),
            MenuItem::action("Enclosing Folder", GoUp),
            MenuItem::action("Top Level", GoToTop),
            MenuItem::separator(),
            MenuItem::action("Reveal in Finder", RevealInFinder),
        ]),
    ]
}

fn quit(_: &Quit, cx: &mut App) {
    telemetry::flush(FLUSH_TIMEOUT);
    cx.quit();
}
