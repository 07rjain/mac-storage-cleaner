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
    pub accent: Rgba,
    pub track: Rgba,
    pub chart_center: Rgba,
}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self {
                dark: true,
                background: rgb(0x1c1c1e),
                panel: rgb(0x242426),
                text: rgb(0xebebf0),
                muted: rgb(0x98989f),
                border: rgb(0x3a3a3c),
                hover: rgb(0x2f2f32),
                selected: rgb(0x0a4a8f),
                accent: rgb(0x0a84ff),
                track: rgb(0x3a3a3c),
                chart_center: rgb(0x2c2c2e),
            },
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self {
                dark: false,
                background: rgb(0xf5f5f7),
                panel: rgb(0xffffff),
                text: rgb(0x1d1d1f),
                muted: rgb(0x6e6e73),
                border: rgb(0xd2d2d7),
                hover: rgb(0xececf0),
                selected: rgb(0xcfe3ff),
                accent: rgb(0x007aff),
                track: rgb(0xe3e3e8),
                chart_center: rgb(0xffffff),
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
