//! The first-launch window: what the app reads, Full Disk Access and crash reports.

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, FontWeight, Render, SharedString, Subscription,
    Window, actions, div, prelude::*,
};

use crate::settings::{self, Settings};
use crate::storage_view::Scope;
use crate::theme::Theme;
use crate::widgets::{badge, button, checkbox, primary_button};
use crate::{APP_NAME, ToggleCrashReports};

actions!(welcome, [StartScanning]);

pub struct Welcome {
    focus_handle: FocusHandle,
    scope: Scope,
    check_access: fn() -> bool,
    full_disk_access: bool,
    opened_settings: bool,
    _activation: Subscription,
}

impl Welcome {
    pub fn new(
        scope: Scope,
        check_access: fn() -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let activation = cx.observe_window_activation(window, |welcome, window, cx| {
            if window.is_window_active() {
                welcome.full_disk_access = (welcome.check_access)();
                cx.notify();
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            scope,
            check_access,
            full_disk_access: check_access(),
            opened_settings: false,
            _activation: activation,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    fn start(&mut self, _: &StartScanning, window: &mut Window, cx: &mut Context<Self>) {
        settings::update(cx, |settings| settings.onboarded = true);
        crate::open_main_window(self.scope.clone(), self.check_access, cx);
        window.remove_window();
    }

    fn render_section(
        title: &'static str,
        status: Option<AnyElement>,
        body: impl Into<SharedString>,
        theme: &Theme,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .children(status),
            )
            .child(div().text_color(theme.muted).child(body.into()))
    }
}

impl Render for Welcome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        let crash_reports = cx.global::<Settings>().crash_reports;
        let status = if self.full_disk_access {
            badge("On", theme.safe)
        } else {
            badge("Off", theme.review)
        };

        let mut access = Self::render_section(
            "Full Disk Access",
            Some(status.into_any_element()),
            "Lets the app measure folders macOS protects, such as Mail, Messages and other apps' \
             data. Without it, those folders show as \u{201c}Not measured\u{201d}.",
            &theme,
        );
        if !self.full_disk_access {
            access = access.child(div().pt_1().flex().child(
                button("open-full-disk-access", "Open Privacy & Security…", &theme).on_click(
                    cx.listener(|welcome, _: &ClickEvent, _, cx| {
                        welcome.opened_settings = true;
                        crate::access::open_settings(cx);
                        cx.notify();
                    }),
                ),
            ));
            if self.opened_settings {
                access = access.child(div().text_xs().text_color(theme.muted).child(format!(
                    "Turn on {APP_NAME} in the list. If macOS asks, choose Quit & Reopen."
                )));
            }
        }

        let crash = Self::render_section(
            "Crash reports",
            None,
            "Crash reports help fix bugs. File names, folder names and your user name are removed \
             before anything is sent. You can change this later in Settings.",
            &theme,
        )
        .child(
            div().pt_1().child(
                checkbox("crash-reports", "Send crash reports", crash_reports, &theme).on_click(
                    |_, window, cx| window.dispatch_action(Box::new(ToggleCrashReports), cx),
                ),
            ),
        );

        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::start))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .text_sm()
            .child(
                div()
                    .flex_1()
                    .px_8()
                    .pt_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child(format!("Welcome to {APP_NAME}")),
                            )
                            .child(
                                div()
                                    .text_color(theme.muted)
                                    .child("See what fills your disk, and clean it up safely."),
                            ),
                    )
                    .child(Self::render_section(
                        "What it reads",
                        None,
                        "The names and sizes of files on this Mac. It doesn't read what's inside \
                         them, and it never downloads files that are only in iCloud.",
                        &theme,
                    ))
                    .child(Self::render_section(
                        "Nothing goes without your review",
                        None,
                        "You choose what to clean. Items go to the Trash, so you can put them back \
                         until you empty it.",
                        &theme,
                    ))
                    .child(access)
                    .child(crash),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .px_8()
                    .py_4()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        primary_button("start", "Start Scanning", theme.accent, true).on_click(
                            |_, window, cx| window.dispatch_action(Box::new(StartScanning), cx),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use gpui::{TestAppContext, VisualTestContext, WindowHandle};

    use super::*;
    use crate::storage_view::StorageView;

    static GRANTED: AtomicBool = AtomicBool::new(false);

    fn granted() -> bool {
        GRANTED.load(Ordering::Relaxed)
    }

    fn open(folder: &std::path::Path, cx: &mut TestAppContext) -> WindowHandle<Welcome> {
        cx.update(|cx| crate::init(Settings::default(), cx));
        let scope = Scope::Folder(folder.to_path_buf());
        let window = cx.add_window(|window, cx| Welcome::new(scope, granted, window, cx));
        window
            .update(cx, |welcome, window, cx| {
                let focus = welcome.focus_handle().clone();
                window.focus(&focus, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
    }

    #[gpui::test]
    fn full_disk_access_is_checked_again_when_the_window_comes_back(cx: &mut TestAppContext) {
        let folder = tempfile::tempdir().unwrap();
        GRANTED.store(false, Ordering::Relaxed);
        let window = open(folder.path(), cx);
        let access = |cx: &mut TestAppContext| {
            window
                .read_with(cx, |welcome, _| welcome.full_disk_access)
                .unwrap()
        };
        assert!(!access(cx));

        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.deactivate_window();
        GRANTED.store(true, Ordering::Relaxed);
        window
            .update(cx, |_, window, _| window.activate_window())
            .unwrap();
        cx.run_until_parked();
        assert!(access(cx));
    }

    #[gpui::test]
    fn start_scanning_finishes_onboarding_and_opens_the_main_window(cx: &mut TestAppContext) {
        let folder = tempfile::tempdir().unwrap();
        let window = open(folder.path(), cx);

        cx.dispatch_action(window.into(), ToggleCrashReports);
        assert!(!cx.read(|cx| cx.global::<Settings>().crash_reports));

        cx.dispatch_action(window.into(), StartScanning);
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(cx.global::<Settings>().onboarded);
            let windows = cx.windows();
            assert_eq!(windows.len(), 1, "the welcome window closed");
            assert!(windows[0].downcast::<StorageView>().is_some());
        });
    }
}
