//! Small controls shared by every window.

use gpui::{Div, ElementId, Rgba, SharedString, Stateful, div, prelude::*, px, white};

use crate::theme::Theme;

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.panel)
        .cursor_pointer()
        .whitespace_nowrap()
        .hover(move |style| style.bg(hover))
        .child(label.into())
}

pub fn primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    color: Rgba,
    enabled: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .whitespace_nowrap()
        .text_color(white())
        .bg(color)
        .when(enabled, |this| this.cursor_pointer())
        .when(!enabled, |this| this.opacity(0.45))
        .child(label.into())
}

pub fn badge(label: impl Into<SharedString>, color: Rgba) -> impl IntoElement {
    div()
        .flex_none()
        .px_1p5()
        .rounded_sm()
        .text_xs()
        .text_color(color)
        .border_1()
        .border_color(color)
        .child(label.into())
}

/// A check box with its label; the whole row is clickable.
pub fn checkbox(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    checked: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let mark = div()
        .flex_none()
        .size(px(16.))
        .rounded_sm()
        .border_1()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .when(checked, |this| {
            this.bg(theme.accent)
                .border_color(theme.accent)
                .text_color(white())
                .child("✓")
        })
        .when(!checked, |this| {
            this.border_color(theme.muted).bg(theme.panel)
        });
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .child(mark)
        .child(label.into())
}
