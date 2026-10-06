//! The About window: version, changelog, licenses and the update check.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use gpui::{Context, FontWeight, IntoElement, Render, SharedString, Task, Window, div, prelude::*};

use crate::theme::Theme;
use crate::update::{self, Release};
use crate::widgets::{button, heading, label, primary_button};
use crate::{APP_NAME, VERSION};

pub struct About {
    state: Check,
    _task: Option<Task<()>>,
}

enum Check {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Failed(SharedString),
}

impl About {
    pub fn new() -> Self {
        Self {
            state: Check::Idle,
            _task: None,
        }
    }

    pub fn check(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state, Check::Checking | Check::Installing(_)) {
            return;
        }
        self.state = Check::Checking;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { update::fetch_latest() })
                .await;
            let _ = this.update(cx, |about, cx| {
                about.state = match result {
                    Ok(release) if update::is_newer(&release.version, VERSION) => {
                        Check::Available(release)
                    }
                    Ok(_) => Check::UpToDate,
                    Err(message) => Check::Failed(message.into()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        let Check::Available(release) = &self.state else {
            return;
        };
        let Some(url) = release.dmg.clone() else {
            return;
        };
        let version = release.version.clone();
        self.state = Check::Installing(release.clone());
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { update::install_update(&url, &version) })
                .await;
            let _ = this.update(cx, |about, cx| match result {
                Ok(()) => {
                    telemetry::flush(Duration::from_secs(2));
                    cx.quit();
                }
                Err(message) => {
                    about.state = Check::Failed(message.into());
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }
}

/// A document shipped in the app bundle's Resources folder, or in the source tree when the app
/// runs from `cargo run`.
fn resource(name: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let bundled = executable.parent()?.parent()?.join("Resources").join(name);
    if bundled.exists() {
        return Some(bundled);
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name);
    source.exists().then_some(source)
}

fn open_document(name: &str) {
    let Some(path) = resource(name) else {
        tracing::warn!("{name} is missing from the app");
        return;
    };
    if let Err(error) = Command::new("/usr/bin/open").arg("-e").arg(path).spawn() {
        tracing::warn!("couldn't open {name}: {error}");
    }
}

impl Render for About {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        let documents = [
            ("changelog", "Changelog", "CHANGELOG.md"),
            ("license", "License", "LICENSE"),
            ("notices", "Third-Party Notices", "THIRD_PARTY_NOTICES.md"),
        ];
        let checking = matches!(self.state, Check::Checking);
        let status = match &self.state {
            Check::Idle => format!("Version {VERSION}"),
            Check::Checking => format!("Version {VERSION} · Checking…"),
            Check::UpToDate => format!("Version {VERSION} is the latest."),
            Check::Available(release) => format!("Version {} is available.", release.version),
            Check::Installing(release) => format!("Installing version {}…", release.version),
            Check::Failed(message) => format!("Version {VERSION} · {message}"),
        };
        let updating = matches!(self.state, Check::Installing(_));
        let update = match &self.state {
            Check::Available(release) | Check::Installing(release) if release.dmg.is_some() => {
                Some(())
            }
            _ => None,
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .px_5()
            .bg(theme.background)
            .text_color(theme.text)
            .text_sm()
            .child(
                heading("name", APP_NAME)
                    .aria_level(1)
                    .text_lg()
                    .font_weight(FontWeight::BOLD),
            )
            .child(
                label("version", status)
                    .text_center()
                    .text_color(theme.muted),
            )
            .child(
                label("copyright", "© 2026 Rishabh · MIT License")
                    .text_xs()
                    .text_color(theme.muted),
            )
            .child(
                div()
                    .pt_3()
                    .flex()
                    .gap_2()
                    .child({
                        let label = if checking {
                            "Checking…"
                        } else {
                            "Check for Updates"
                        };
                        button("check-updates", label, &theme).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.check(cx);
                            },
                        ))
                    })
                    .when_some(update, |this, _| {
                        let color = theme.accent_fill;
                        let label = if updating { "Updating…" } else { "Update" };
                        this.child(
                            primary_button("install-update", label, color, !updating).when(
                                !updating,
                                |this| {
                                    this.on_click(cx.listener(|this, _, _, cx| {
                                        this.install(cx);
                                    }))
                                },
                            ),
                        )
                    }),
            )
            .when(
                matches!(self.state, Check::Available(_) | Check::Installing(_)),
                |this| {
                this.child(
                    label(
                        "update-hint",
                        "Replaces this app and reopens it. Full Disk Access has to be granted again.",
                    )
                    .text_center()
                    .text_xs()
                    .text_color(theme.muted)
                    .pt_1(),
                )
            })
            .child(
                div()
                    .pt_3()
                    .flex()
                    .gap_2()
                    .children(documents.map(|(id, label, file)| {
                        button(id, label, &theme).on_click(move |_, _, _| open_document(file))
                    })),
            )
    }
}
