mod about;
mod access;
mod compare;
mod file_types;
mod format;
mod icicle;
mod model;
mod preferences;
mod settings;
mod storage_view;
mod sunburst;
mod theme;
mod treemap;
mod types;
mod update;
mod welcome;
mod widgets;

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Global, KeyBinding, Menu, MenuItem, Pixels, Size,
    TitlebarOptions, WindowBounds, WindowOptions, actions, px, size,
};
use gpui_platform::application;
use tracing_subscriber::prelude::*;

use crate::about::About;
use crate::preferences::Preferences;
use crate::settings::{Chart, Settings};
use crate::storage_view::{
    AddToBasket, CollapseOrUp, Dismiss, EmptyBasket, ExpandOrOpen, GoToTop, GoUp, OpenSelected,
    QuickLook, Rescan, RevealInFinder, ReviewBasket, ScanFolder, ScanHomeFolder, ScanStartupDisk,
    Scope, SelectNext, SelectPrevious, ShowHistory, StopScan, StorageView,
};
use crate::welcome::Welcome;

pub const APP_ID: &str = "mac-storage-cleaner";
const APP_NAME: &str = "Mac Storage Cleaner";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

actions!(
    app,
    [
        Quit,
        ShowAbout,
        CheckForUpdates,
        ShowSettings,
        ToggleCrashReports,
        ShowSunburst,
        ShowTreemap,
        ShowIcicle,
        ShowTree,
        ShowTypes
    ]
);

/// The Settings and About windows, so each opens at most once.
#[derive(Default)]
struct Panels {
    settings: Option<AnyWindowHandle>,
    about: Option<AnyWindowHandle>,
}

impl Global for Panels {}

fn main() {
    let settings = Settings::load();
    let _telemetry = telemetry::init(telemetry::Config {
        dsn: option_env!("SENTRY_DSN"),
        release: format!("{APP_ID}@{VERSION}"),
        enabled: settings.crash_reports,
        crash_file: settings::data_dir().map(|dir| dir.join("native-crash.bin")),
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
        let onboarded = settings.onboarded;
        init(settings, cx);
        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                quit(&Quit, cx);
            }
        })
        .detach();

        if onboarded {
            open_main_window(scope, access::has_full_disk_access, cx);
        } else {
            open_welcome_window(scope, cx);
        }
    });
}

/// Settings, app-wide actions, key bindings and menus.
fn init(settings: Settings, cx: &mut App) {
    cx.set_menus(menus(&settings));
    cx.set_global(settings);
    cx.set_global(Panels::default());
    cx.on_action(quit);
    cx.on_action(|_: &ToggleCrashReports, cx| {
        settings::update(cx, |settings| {
            settings.crash_reports = !settings.crash_reports
        });
    });
    cx.on_action(|_: &ShowSunburst, cx| {
        settings::update(cx, |settings| settings.chart = Chart::Sunburst)
    });
    cx.on_action(|_: &ShowTreemap, cx| {
        settings::update(cx, |settings| settings.chart = Chart::Treemap)
    });
    cx.on_action(|_: &ShowIcicle, cx| {
        settings::update(cx, |settings| settings.chart = Chart::Icicle)
    });
    cx.on_action(|_: &ShowTree, cx| settings::update(cx, |settings| settings.chart = Chart::Tree));
    cx.on_action(|_: &ShowTypes, cx| {
        settings::update(cx, |settings| settings.chart = Chart::Types)
    });
    cx.on_action(|_: &ShowSettings, cx| open_settings_window(cx));
    cx.on_action(|_: &ShowAbout, cx| open_about_window(false, cx));
    cx.on_action(|_: &CheckForUpdates, cx| open_about_window(true, cx));
    cx.bind_keys(key_bindings());
}

fn window_options(title: &str, size: Size<Pixels>, resizable: bool, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.to_string().into()),
            ..Default::default()
        }),
        is_resizable: resizable,
        is_minimizable: resizable,
        ..Default::default()
    }
}

fn open_failed(name: &str, error: impl std::fmt::Display, cx: &mut App) {
    tracing::error!("failed to open the {name} window: {error:#}");
    if cx.windows().is_empty() {
        quit(&Quit, cx);
    }
}

pub fn open_main_window(scope: Scope, check_access: fn() -> bool, cx: &mut App) {
    let options = WindowOptions {
        window_min_size: Some(size(px(820.), px(560.))),
        ..window_options(APP_NAME, size(px(1180.), px(780.)), true, cx)
    };
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| {
            let mut view = StorageView::new(scope, cx);
            view.watch_full_disk_access(check_access, window, cx);
            view
        });
        let focus = view.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        view
    });
    match opened {
        Ok(_) => cx.activate(true),
        Err(error) => open_failed("main", error, cx),
    }
}

fn open_welcome_window(scope: Scope, cx: &mut App) {
    let options = window_options(APP_NAME, size(px(620.), px(640.)), false, cx);
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| Welcome::new(scope, access::has_full_disk_access, window, cx));
        let focus = view.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        view
    });
    match opened {
        Ok(_) => cx.activate(true),
        Err(error) => open_failed("welcome", error, cx),
    }
}

/// Brings an open panel to the front. Returns false if it was closed.
fn activate(handle: Option<AnyWindowHandle>, cx: &mut App) -> bool {
    handle.is_some_and(|handle| {
        handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    })
}

fn open_settings_window(cx: &mut App) {
    if activate(cx.global::<Panels>().settings, cx) {
        return;
    }
    let options = window_options("Settings", size(px(520.), px(420.)), false, cx);
    match cx.open_window(options, |window, cx| {
        cx.new(|cx| Preferences::new(window, cx))
    }) {
        Ok(handle) => cx.global_mut::<Panels>().settings = Some(handle.into()),
        Err(error) => open_failed("settings", error, cx),
    }
}

fn open_about_window(check: bool, cx: &mut App) {
    if let Some(handle) = cx
        .global::<Panels>()
        .about
        .and_then(|handle| handle.downcast::<About>())
    {
        let updated = handle.update(cx, |about, window: &mut gpui::Window, cx| {
            window.activate_window();
            if check {
                about.check(cx);
            }
        });
        if updated.is_ok() {
            return;
        }
    }
    let options = window_options(
        &format!("About {APP_NAME}"),
        size(px(480.), px(360.)),
        false,
        cx,
    );
    match cx.open_window(options, |_, cx| {
        cx.new(|cx| {
            let mut about = About::new();
            if check {
                about.check(cx);
            }
            about
        })
    }) {
        Ok(handle) => cx.global_mut::<Panels>().about = Some(handle.into()),
        Err(error) => open_failed("about", error, cx),
    }
}

fn key_bindings() -> Vec<KeyBinding> {
    let view = Some("StorageView");
    vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", ShowSettings, None),
        KeyBinding::new("down", SelectNext, view),
        KeyBinding::new("up", SelectPrevious, view),
        KeyBinding::new("enter", OpenSelected, view),
        KeyBinding::new("right", ExpandOrOpen, view),
        KeyBinding::new("cmd-down", OpenSelected, view),
        KeyBinding::new("left", CollapseOrUp, view),
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
        KeyBinding::new("alt-cmd-1", ShowSunburst, None),
        KeyBinding::new("alt-cmd-2", ShowTreemap, None),
        KeyBinding::new("alt-cmd-3", ShowIcicle, None),
        KeyBinding::new("alt-cmd-4", ShowTree, None),
        KeyBinding::new("alt-cmd-5", ShowTypes, None),
        KeyBinding::new("cmd-backspace", AddToBasket, view),
        KeyBinding::new("cmd-b", ReviewBasket, view),
        KeyBinding::new("space", QuickLook, view),
        KeyBinding::new("cmd-y", QuickLook, view),
    ]
}

pub fn menus(settings: &Settings) -> Vec<Menu> {
    vec![
        Menu::new(APP_NAME).items([
            MenuItem::action(format!("About {APP_NAME}"), ShowAbout),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Settings…", ShowSettings),
            MenuItem::action("Send Crash Reports", ToggleCrashReports)
                .checked(settings.crash_reports),
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
        Menu::new("View").items([
            MenuItem::action("As Sunburst", ShowSunburst)
                .checked(settings.chart == Chart::Sunburst),
            MenuItem::action("As Treemap", ShowTreemap).checked(settings.chart == Chart::Treemap),
            MenuItem::action("As Icicle", ShowIcicle).checked(settings.chart == Chart::Icicle),
            MenuItem::action("As Tree", ShowTree).checked(settings.chart == Chart::Tree),
            MenuItem::action("As File Types", ShowTypes).checked(settings.chart == Chart::Types),
        ]),
        Menu::new("Go").items([
            MenuItem::action("Open Folder", OpenSelected),
            MenuItem::action("Enclosing Folder", GoUp),
            MenuItem::action("Top Level", GoToTop),
            MenuItem::separator(),
            MenuItem::action("Reveal in Finder", RevealInFinder),
            MenuItem::action("Quick Look", QuickLook),
        ]),
        Menu::new("Clean").items([
            MenuItem::action("Add to Basket / Remove", AddToBasket),
            MenuItem::action("Review Basket…", ReviewBasket),
            MenuItem::action("Empty Basket", EmptyBasket),
            MenuItem::separator(),
            MenuItem::action("Cleanup History…", ShowHistory),
        ]),
    ]
}

fn quit(_: &Quit, cx: &mut App) {
    telemetry::flush(FLUSH_TIMEOUT);
    cx.quit();
}
