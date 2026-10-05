//! The Settings window.

use std::process::Command;

use gpui::{Context, ElementId, Render, Role, SharedString, Subscription, Window, div, prelude::*};

use crate::settings::{self, Settings};
use crate::theme::Theme;
use crate::widgets::{badge, button, checkbox, heading, label};
use crate::{ToggleCrashReports, access};

pub struct Preferences {
    full_disk_access: bool,
    _activation: Subscription,
}

impl Preferences {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activation = cx.observe_window_activation(window, |preferences, window, cx| {
            if window.is_window_active() {
                preferences.full_disk_access = access::has_full_disk_access();
                cx.notify();
            }
        });
        Self {
            full_disk_access: access::has_full_disk_access(),
            _activation: activation,
        }
    }
}

fn section(title: &'static str, body: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .pb_4()
        .border_b_1()
        .border_color(theme.border)
        .child(heading(title, title))
        .child(label(ElementId::Name(format!("{title} body").into()), body).text_color(theme.muted))
}

impl Render for Preferences {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        let crash_reports = cx.global::<Settings>().crash_reports;
        let log = settings::cleanup_log_path().filter(|path| path.exists());

        let crash = section(
            "Crash reports",
            "Sent when the app crashes or hits an unexpected error. File names, folder names and \
             your user name are removed before anything is sent.",
            &theme,
        )
        .child(
            checkbox("crash-reports", "Send crash reports", crash_reports, &theme)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleCrashReports), cx)),
        );

        let (state, color) = if self.full_disk_access {
            ("On", theme.safe)
        } else {
            ("Off", theme.review)
        };
        let full_disk_access = section(
            "Full Disk Access",
            "Lets the app measure folders macOS protects. After installing a new version, you may \
             need to turn it on again.",
            &theme,
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("full-disk-access-status")
                        .role(Role::Label)
                        .aria_label(format!("Full Disk Access is {}", state.to_lowercase()))
                        .child(badge(state, color)),
                )
                .child(
                    button("open-full-disk-access", "Open Privacy & Security…", &theme)
                        .on_click(|_, _, cx| access::open_settings(cx)),
                ),
        );

        let history = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(heading("history-title", "Cleanup history"))
            .child(
                label(
                    "history-body",
                    "Every cleanup is logged on this Mac only, with the paths it moved.",
                )
                .text_color(theme.muted),
            )
            .child(
                div().flex().child(match log {
                    Some(path) => button("reveal-log", "Show Log in Finder", &theme)
                        .on_click(move |_, _, _| {
                            if let Err(error) =
                                Command::new("/usr/bin/open").arg("-R").arg(&path).spawn()
                            {
                                tracing::warn!("couldn't reveal the cleanup log: {error}");
                            }
                        })
                        .into_any_element(),
                    None => label("history-empty", "Nothing has been cleaned yet.")
                        .text_xs()
                        .text_color(theme.muted)
                        .into_any_element(),
                }),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .px_6()
            .py_5()
            .bg(theme.background)
            .text_color(theme.text)
            .text_sm()
            .child(crash)
            .child(full_disk_access)
            .child(history)
    }
}
