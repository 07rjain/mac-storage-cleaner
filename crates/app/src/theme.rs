use gpui::{Hsla, Rgba, WindowAppearance, hsla, rgb};

use crate::model::Item;
use crate::sunburst::Segment;

/// Hues for the top-level slices, in order of size. Neighbours are far apart on the wheel.
const HUES: [f32; 10] = [0.58, 0.07, 0.37, 0.97, 0.75, 0.13, 0.49, 0.88, 0.28, 0.66];

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    pub background: Rgba,
    pub panel: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub border: Rgba,
    pub hover: Rgba,
    pub selected: Rgba,
    /// Text and outlines; readable on every surface.
    pub accent: Rgba,
    /// Behind white text: primary buttons and check marks.
    pub accent_fill: Rgba,
    pub track: Rgba,
    pub chart_center: Rgba,
    /// "Safe to delete" badges.
    pub safe: Rgba,
    /// "Review first" badges.
    pub review: Rgba,
    /// Warnings about moving things to the Trash or deleting them.
    pub destructive: Rgba,
    /// Behind the white text of buttons that move things to the Trash or delete them.
    pub destructive_fill: Rgba,
    /// Behind a sheet.
    pub backdrop: Hsla,
}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self {
                dark: true,
                background: rgb(0x1c1c1e),
                panel: rgb(0x242426),
                text: rgb(0xebebf0),
                muted: rgb(0xa1a1a8),
                border: rgb(0x3a3a3c),
                hover: rgb(0x2f2f32),
                selected: rgb(0x183a5c),
                accent: rgb(0x5aaaff),
                accent_fill: rgb(0x0a63d1),
                track: rgb(0x3a3a3c),
                chart_center: rgb(0x2c2c2e),
                safe: rgb(0x30d158),
                review: rgb(0xff9f0a),
                destructive: rgb(0xff6961),
                destructive_fill: rgb(0xc4161c),
                backdrop: hsla(0.0, 0.0, 0.0, 0.5),
            },
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self {
                dark: false,
                background: rgb(0xf5f5f7),
                panel: rgb(0xffffff),
                text: rgb(0x1d1d1f),
                muted: rgb(0x636366),
                border: rgb(0xd2d2d7),
                hover: rgb(0xececf0),
                selected: rgb(0xdbe8fc),
                accent: rgb(0x0064d1),
                accent_fill: rgb(0x0064d1),
                track: rgb(0xe3e3e8),
                chart_center: rgb(0xffffff),
                safe: rgb(0x1e7b34),
                review: rgb(0xb83200),
                destructive: rgb(0xd70015),
                destructive_fill: rgb(0xd70015),
                backdrop: hsla(0.0, 0.0, 0.0, 0.25),
            },
        }
    }

    /// Color of a slice. Rows in the list use the ring-1 color of the same item.
    pub fn slice(&self, segment: &Segment, hovered: bool) -> Hsla {
        let lift = if hovered { 0.1 } else { 0.0 };
        let neutral = |lightness_light: f32, lightness_dark: f32| {
            let lightness = if self.dark {
                lightness_dark
            } else {
                lightness_light
            };
            hsla(0.62, 0.06, lightness + lift, 1.0)
        };
        match segment.item {
            Item::OtherVolumes => neutral(0.62, 0.42),
            Item::NotMeasured => neutral(0.82, 0.30),
            Item::Smaller(_) => neutral(0.72, 0.36),
            Item::Node(_) => {
                let hue = HUES[segment.branch % HUES.len()];
                let depth = f32::from(segment.ring - 1);
                let (base, step) = if self.dark {
                    (0.46, 0.045)
                } else {
                    (0.50, 0.055)
                };
                let mut saturation = if segment.folder { 0.68 } else { 0.50 };
                let mut lightness = base + depth * step;
                if !segment.settled {
                    saturation *= 0.3;
                    lightness += 0.06;
                }
                hsla(hue, saturation, (lightness + lift).min(0.92), 1.0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::white;

    use super::*;

    /// WCAG 2 contrast ratio, from 1 (none) to 21 (black on white).
    fn contrast(a: Rgba, b: Rgba) -> f32 {
        fn luminance(color: Rgba) -> f32 {
            let channel = |c: f32| {
                if c <= 0.039_28 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
        }
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn text_colors_meet_wcag_aa_contrast() {
        let mut failures = Vec::new();
        for appearance in [WindowAppearance::Light, WindowAppearance::Dark] {
            let theme = Theme::for_appearance(appearance);
            let white: Rgba = white().into();
            let surfaces = [
                ("background", theme.background),
                ("panel", theme.panel),
                ("hover", theme.hover),
                ("selected", theme.selected),
            ];
            let inks = [
                ("text", theme.text),
                ("muted", theme.muted),
                ("accent", theme.accent),
                ("safe", theme.safe),
                ("review", theme.review),
                ("destructive", theme.destructive),
            ];
            // Selected rows only hold names, sizes, notes and "In basket".
            let mut pairs: Vec<_> = inks
                .iter()
                .flat_map(|ink| surfaces.iter().map(move |surface| (*ink, *surface)))
                .filter(|((ink, _), (surface, _))| {
                    *surface != "selected" || matches!(*ink, "text" | "muted" | "accent")
                })
                .collect();
            pairs.push((("white", white), ("accent_fill", theme.accent_fill)));
            pairs.push((
                ("white", white),
                ("destructive_fill", theme.destructive_fill),
            ));
            for ((ink_name, ink), (surface_name, surface)) in pairs {
                let ratio = contrast(ink, surface);
                if ratio < 4.5 {
                    failures.push(format!(
                        "{appearance:?}: {ink_name} on {surface_name} is {ratio:.2}:1"
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "below 4.5:1:\n{}", failures.join("\n"));
    }

    #[test]
    fn contrast_matches_known_values() {
        let black = rgb(0x000000);
        let white = rgb(0xffffff);
        assert!((contrast(black, white) - 21.0).abs() < 0.01);
        assert!((contrast(rgb(0x767676), white) - 4.54).abs() < 0.01);
    }
}
