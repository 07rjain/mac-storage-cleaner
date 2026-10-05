//! The About window: version, changelog and licenses.

use std::path::{Path, PathBuf};
use std::process::Command;

use gpui::{Context, FontWeight, Render, Window, div, prelude::*};

use crate::theme::Theme;
use crate::widgets::{button, heading, label};
use crate::{APP_NAME, VERSION};

pub struct About;

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
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        let documents = [
            ("changelog", "Changelog", "CHANGELOG.md"),
            ("license", "License", "LICENSE"),
            ("notices", "Third-Party Notices", "THIRD_PARTY_NOTICES.md"),
        ];
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .bg(theme.background)
            .text_color(theme.text)
            .text_sm()
            .child(
                heading("name", APP_NAME)
                    .aria_level(1)
                    .text_lg()
                    .font_weight(FontWeight::BOLD),
            )
            .child(label("version", format!("Version {VERSION}")).text_color(theme.muted))
            .child(
                label("copyright", "© 2026 Rishabh · MIT License")
                    .text_xs()
                    .text_color(theme.muted),
            )
            .child(
                div()
                    .pt_4()
                    .flex()
                    .gap_2()
                    .children(documents.map(|(id, label, file)| {
                        button(id, label, &theme).on_click(move |_, _, _| open_document(file))
                    })),
            )
    }
}
