//! Small controls shared by every window.
//!
//! Labels are plain strings, which GPUI leaves out of the accessibility tree,
//! so each control names itself with `aria_label` instead.

use gpui::{
    Div, ElementId, FontWeight, Rgba, Role, SharedString, Stateful, Toggled, div, prelude::*, px,
    white,
};

use crate::theme::Theme;

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.hover;
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.panel)
        .cursor_pointer()
        .whitespace_nowrap()
        .hover(move |style| style.bg(hover))
        .child(label)
}

pub fn primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    color: Rgba,
    enabled: bool,
) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .when(!enabled, |this| this.aria_description("Unavailable"))
        .px_3()
        .py_1()
        .rounded_md()
        .whitespace_nowrap()
        .text_color(white())
        .bg(color)
        .when(enabled, |this| this.cursor_pointer())
        .when(!enabled, |this| this.opacity(0.45))
        .child(label)
}

/// Text that screen readers read.
pub fn label(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    let text = text.into();
    div()
        .id(id)
        .role(Role::Label)
        .aria_label(text.clone())
        .child(text)
}

pub fn heading(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    let text = text.into();
    div()
        .id(id)
        .role(Role::Heading)
        .aria_label(text.clone())
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
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
    let label = label.into();
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
            this.bg(theme.accent_fill)
                .border_color(theme.accent_fill)
                .text_color(white())
                .child("✓")
        })
        .when(!checked, |this| {
            this.border_color(theme.muted).bg(theme.panel)
        });
    div()
        .id(id)
        .role(Role::CheckBox)
        .aria_label(label.clone())
        .aria_toggled(if checked {
            Toggled::True
        } else {
            Toggled::False
        })
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .child(mark)
        .child(label)
}
