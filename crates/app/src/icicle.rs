//! Icicle chart: the focused folder is a full-width bar, and each row below is one level deeper.
//!
//! Widths are the same fractions the sunburst uses for its rings.

use gpui::{
    BorderStyle, Bounds, Hsla, Pixels, Point, Window, fill, point, px, quad, size,
    transparent_black,
};

use crate::sunburst::Segment;

const GAP: f32 = 1.0;

pub enum Hit {
    /// The top bar. Clicking it goes up a level, like the sunburst center.
    Root,
    Segment(usize),
}

/// Rows drawn: the focused folder, plus one row per ring that has a slice.
pub fn row_count(segments: &[Segment]) -> u8 {
    segments
        .iter()
        .map(|segment| segment.ring)
        .max()
        .unwrap_or(0)
        + 1
}

pub fn hit_test(
    segments: &[Segment],
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
) -> Option<Hit> {
    let local_x = f32::from(position.x - bounds.origin.x);
    let local_y = f32::from(position.y - bounds.origin.y);
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width <= 0.0
        || height <= 0.0
        || local_x < 0.0
        || local_y < 0.0
        || local_x >= width
        || local_y >= height
    {
        return None;
    }
    let rows = f32::from(row_count(segments));
    let row = (local_y / (height / rows)).floor() as u8;
    if row == 0 {
        return Some(Hit::Root);
    }
    let fraction = f64::from(local_x / width);
    segments
        .iter()
        .position(|segment| {
            segment.ring == row && segment.start <= fraction && fraction < segment.end
        })
        .map(Hit::Segment)
}

pub struct Paint {
    pub root: Hsla,
    pub colors: Vec<Hsla>,
    /// Slice to outline, and the outline color.
    pub outline: Option<(usize, Hsla)>,
}

pub fn paint(window: &mut Window, bounds: Bounds<Pixels>, segments: &[Segment], style: &Paint) {
    let rows = f32::from(row_count(segments));
    let width = f32::from(bounds.size.width);
    let row_h = f32::from(bounds.size.height) / rows;
    let gap = px(GAP);

    let mut root = Bounds::new(bounds.origin, size(bounds.size.width, px(row_h)));
    inset(&mut root, gap);
    window.paint_quad(fill(root, style.root));

    for (segment, color) in segments.iter().zip(&style.colors) {
        let mut rect = band(bounds, width, row_h, segment);
        inset(&mut rect, gap);
        if rect.size.width >= px(0.5) && rect.size.height >= px(0.5) {
            window.paint_quad(fill(rect, *color));
        }
    }

    if let Some((index, color)) = style.outline
        && let Some(segment) = segments.get(index)
    {
        window.paint_quad(quad(
            band(bounds, width, row_h, segment),
            px(0.0),
            transparent_black(),
            px(2.0),
            color,
            BorderStyle::Solid,
        ));
    }
}

fn band(bounds: Bounds<Pixels>, width: f32, row_h: f32, segment: &Segment) -> Bounds<Pixels> {
    let span = (segment.end - segment.start).max(0.0) as f32;
    Bounds::new(
        point(
            bounds.origin.x + px(segment.start as f32 * width),
            bounds.origin.y + px(f32::from(segment.ring) * row_h),
        ),
        size(px(span * width), px(row_h)),
    )
}

fn inset(bounds: &mut Bounds<Pixels>, gap: Pixels) {
    if bounds.size.width > gap * 2.0 && bounds.size.height > gap * 2.0 {
        bounds.origin.x += gap;
        bounds.origin.y += gap;
        bounds.size.width -= gap * 2.0;
        bounds.size.height -= gap * 2.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Item;
    use gpui::{Bounds, point, px, size};

    fn segment(ring: u8, start: f64, end: f64) -> Segment {
        Segment {
            item: Item::NotMeasured,
            ring,
            start,
            end,
            branch: 0,
            size: 0,
            settled: true,
            folder: true,
        }
    }

    fn bounds() -> Bounds<Pixels> {
        Bounds::new(point(px(10.), px(20.)), size(px(200.), px(100.)))
    }

    #[test]
    fn the_top_row_is_the_folder_and_a_lower_row_hits_its_slice() {
        let segments = vec![
            segment(1, 0.0, 0.4),
            segment(1, 0.4, 1.0),
            segment(2, 0.0, 0.4),
        ];
        let bounds = bounds();
        let root = point(px(50.), px(30.));
        assert!(matches!(hit_test(&segments, bounds, root), Some(Hit::Root)));

        let right = point(px(10. + 150.), px(20. + 50.));
        assert!(matches!(
            hit_test(&segments, bounds, right),
            Some(Hit::Segment(1))
        ));

        let nested = point(px(40.), px(20. + 80.));
        assert!(matches!(
            hit_test(&segments, bounds, nested),
            Some(Hit::Segment(2))
        ));
    }

    #[test]
    fn points_outside_the_chart_miss() {
        let segments = vec![segment(1, 0.0, 1.0)];
        assert!(hit_test(&segments, bounds(), point(px(0.), px(20.))).is_none());
    }
}
