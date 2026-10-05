//! Sunburst chart: the focused folder is the center, and each ring is one level deeper.
//!
//! Angles are fractions of a full turn, clockwise from 12 o'clock.

use std::cmp::Reverse;
use std::f64::consts::TAU;

use gpui::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, point, px};
use scanner::{NodeId, Tree};

use crate::model::{self, Item, Row};

pub const RINGS: u8 = 6;
/// Slices narrower than half a degree are merged into one "Smaller items" slice.
const MIN_FRACTION: f64 = 0.5 / 360.0;
/// Gap between neighbouring slices and rings, in pixels.
const GAP: f64 = 1.0;
/// Line segments per degree of arc.
const STEPS_PER_DEGREE: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub item: Item,
    /// 1 is the innermost ring.
    pub ring: u8,
    pub start: f64,
    pub end: f64,
    /// Index of the ring-1 slice this one belongs to, which picks its color.
    pub branch: usize,
    pub size: u64,
    pub settled: bool,
    pub folder: bool,
}

/// Lays out `folder`. `ring_one` are its rows as shown in the list, largest first.
pub fn layout(tree: &Tree, folder: NodeId, ring_one: &[Row]) -> Vec<Segment> {
    let total: u64 = ring_one.iter().map(|row| row.size).sum();
    let mut segments = Vec::new();
    if total == 0 {
        return segments;
    }
    let scale = 1.0 / total as f64;
    let mut angle = 0.0;
    let mut smaller = 0;
    for (branch, row) in ring_one.iter().enumerate() {
        let span = row.size as f64 * scale;
        if span < MIN_FRACTION {
            smaller += row.size;
            continue;
        }
        let is_folder = model::is_folder(tree, row.item);
        segments.push(Segment {
            item: row.item,
            ring: 1,
            start: angle,
            end: angle + span,
            branch,
            size: row.size,
            settled: row.settled,
            folder: is_folder,
        });
        if let (Item::Node(id), true) = (row.item, is_folder) {
            place_children(tree, id, 2, angle, scale, branch, &mut segments);
        }
        angle += span;
    }
    push_smaller(
        tree,
        folder,
        1,
        angle,
        smaller,
        scale,
        ring_one.len(),
        &mut segments,
    );
    segments
}

fn place_children(
    tree: &Tree,
    parent: NodeId,
    ring: u8,
    start: f64,
    scale: f64,
    branch: usize,
    segments: &mut Vec<Segment>,
) {
    if ring > RINGS {
        return;
    }
    let mut drawn: Vec<(NodeId, u64)> = tree
        .children(parent)
        .filter_map(|child| {
            let size = tree.allocated(child);
            (size as f64 * scale >= MIN_FRACTION).then_some((child, size))
        })
        .collect();
    drawn.sort_unstable_by_key(|&(child, size)| (Reverse(size), child));

    let mut angle = start;
    let mut drawn_total = 0;
    for (child, size) in drawn {
        let span = size as f64 * scale;
        let is_folder = model::is_folder(tree, Item::Node(child));
        segments.push(Segment {
            item: Item::Node(child),
            ring,
            start: angle,
            end: angle + span,
            branch,
            size,
            settled: tree.is_settled(child),
            folder: is_folder,
        });
        if is_folder {
            place_children(tree, child, ring + 1, angle, scale, branch, segments);
        }
        angle += span;
        drawn_total += size;
    }
    let rest = tree.allocated(parent).saturating_sub(drawn_total);
    push_smaller(tree, parent, ring, angle, rest, scale, branch, segments);
}

#[allow(clippy::too_many_arguments)]
fn push_smaller(
    tree: &Tree,
    parent: NodeId,
    ring: u8,
    start: f64,
    size: u64,
    scale: f64,
    branch: usize,
    segments: &mut Vec<Segment>,
) {
    let span = size as f64 * scale;
    if span >= MIN_FRACTION {
        segments.push(Segment {
            item: Item::Smaller(parent),
            ring,
            start,
            end: start + span,
            branch,
            size,
            settled: tree.is_settled(parent),
            folder: false,
        });
    }
}

/// Where the chart sits inside its bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub center_x: f64,
    pub center_y: f64,
    /// Radius of the center circle.
    pub hole: f64,
    pub ring_width: f64,
}

impl Geometry {
    pub fn fit(bounds: Bounds<Pixels>) -> Self {
        let width = f64::from(f32::from(bounds.size.width));
        let height = f64::from(f32::from(bounds.size.height));
        let radius = (width.min(height) / 2.0 - 16.0).max(48.0);
        let hole = radius * 0.26;
        Self {
            center_x: f64::from(f32::from(bounds.origin.x)) + width / 2.0,
            center_y: f64::from(f32::from(bounds.origin.y)) + height / 2.0,
            hole,
            ring_width: (radius - hole) / f64::from(RINGS),
        }
    }

    pub fn inner_radius(&self, ring: u8) -> f64 {
        self.hole + self.ring_width * f64::from(ring - 1)
    }

    pub fn point(&self, fraction: f64, radius: f64) -> Point<Pixels> {
        let angle = fraction * TAU;
        point(
            px((self.center_x + radius * angle.sin()) as f32),
            px((self.center_y - radius * angle.cos()) as f32),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Center,
    Segment(usize),
}

pub fn hit_test(segments: &[Segment], geometry: Geometry, position: Point<Pixels>) -> Option<Hit> {
    let dx = f64::from(f32::from(position.x)) - geometry.center_x;
    let dy = f64::from(f32::from(position.y)) - geometry.center_y;
    let distance = dx.hypot(dy);
    if distance < geometry.hole {
        return Some(Hit::Center);
    }
    let ring = ((distance - geometry.hole) / geometry.ring_width).floor() as i64 + 1;
    if ring > i64::from(RINGS) {
        return None;
    }
    let fraction = (dx.atan2(-dy) / TAU).rem_euclid(1.0);
    segments
        .iter()
        .position(|segment| {
            i64::from(segment.ring) == ring && segment.start <= fraction && fraction < segment.end
        })
        .map(Hit::Segment)
}

/// Outline of one slice, with half a gap trimmed from every side when it is wide enough.
fn outline(geometry: Geometry, segment: &Segment) -> Vec<Point<Pixels>> {
    let inner = geometry.inner_radius(segment.ring) + GAP / 2.0;
    let outer = geometry.inner_radius(segment.ring) + geometry.ring_width - GAP / 2.0;
    let trim = |radius: f64| {
        let trim = GAP / 2.0 / (TAU * radius);
        if segment.end - segment.start > 4.0 * trim && segment.end - segment.start < 1.0 {
            trim
        } else {
            0.0
        }
    };
    let steps = (((segment.end - segment.start) * 360.0 * STEPS_PER_DEGREE).ceil() as usize).max(1);
    let mut points = Vec::with_capacity(2 * (steps + 1));
    let (outer_trim, inner_trim) = (trim(outer), trim(inner));
    for step in 0..=steps {
        let t = step as f64 / steps as f64;
        let fraction =
            segment.start + outer_trim + t * (segment.end - segment.start - 2.0 * outer_trim);
        points.push(geometry.point(fraction, outer));
    }
    for step in (0..=steps).rev() {
        let t = step as f64 / steps as f64;
        let fraction =
            segment.start + inner_trim + t * (segment.end - segment.start - 2.0 * inner_trim);
        points.push(geometry.point(fraction, inner));
    }
    points
}

fn circle(geometry: Geometry, radius: f64) -> Vec<Point<Pixels>> {
    (0..120)
        .map(|step| geometry.point(f64::from(step) / 120.0, radius))
        .collect()
}

pub struct Paint {
    pub center: Hsla,
    pub colors: Vec<Hsla>,
    /// Slice to outline, and the outline color.
    pub outline: Option<(usize, Hsla)>,
}

pub fn paint(window: &mut Window, geometry: Geometry, segments: &[Segment], style: &Paint) {
    let mut center = PathBuilder::fill();
    center.add_polygon(&circle(geometry, geometry.hole - GAP / 2.0), true);
    if let Ok(path) = center.build() {
        window.paint_path(path, style.center);
    }

    for (segment, color) in segments.iter().zip(&style.colors) {
        let mut builder = PathBuilder::fill();
        builder.add_polygon(&outline(geometry, segment), true);
        if let Ok(path) = builder.build() {
            window.paint_path(path, *color);
        }
    }

    if let Some((index, color)) = style.outline
        && let Some(segment) = segments.get(index)
    {
        let mut builder = PathBuilder::stroke(px(2.0));
        builder.add_polygon(&outline(geometry, segment), true);
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    fn segment(ring: u8, start: f64, end: f64) -> Segment {
        Segment {
            item: Item::NotMeasured,
            ring,
            start,
            end,
            branch: 0,
            size: 0,
            settled: true,
            folder: false,
        }
    }

    fn geometry() -> Geometry {
        Geometry::fit(Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(432.0), px(432.0)),
        })
    }

    #[test]
    fn fits_the_smaller_side() {
        let geometry = geometry();
        assert_eq!((geometry.center_x, geometry.center_y), (216.0, 216.0));
        let radius = geometry.inner_radius(RINGS) + geometry.ring_width;
        assert!((radius - 200.0).abs() < 1e-9);
    }

    #[test]
    fn hit_test_maps_angle_and_radius_to_a_slice() {
        let geometry = geometry();
        let segments = [
            segment(1, 0.0, 0.25),
            segment(1, 0.25, 1.0),
            segment(2, 0.0, 0.1),
        ];
        let ring_one = geometry.hole + geometry.ring_width / 2.0;
        let ring_two = geometry.inner_radius(2) + geometry.ring_width / 2.0;
        let at = |fraction: f64, radius: f64| geometry.point(fraction, radius);

        assert_eq!(
            hit_test(&segments, geometry, at(0.0, 5.0)),
            Some(Hit::Center)
        );
        assert_eq!(
            hit_test(&segments, geometry, at(0.1, ring_one)),
            Some(Hit::Segment(0))
        );
        // 3 o'clock is a quarter turn, which starts the second slice.
        assert_eq!(
            hit_test(&segments, geometry, at(0.26, ring_one)),
            Some(Hit::Segment(1))
        );
        assert_eq!(
            hit_test(&segments, geometry, at(0.95, ring_one)),
            Some(Hit::Segment(1))
        );
        assert_eq!(
            hit_test(&segments, geometry, at(0.05, ring_two)),
            Some(Hit::Segment(2))
        );
        assert_eq!(hit_test(&segments, geometry, at(0.5, ring_two)), None);
        assert_eq!(hit_test(&segments, geometry, at(0.5, 300.0)), None);
    }

    #[test]
    fn layout_nests_children_in_their_parent_and_merges_tiny_items() {
        let root = tempfile::tempdir().unwrap();
        let big = root.path().join("big");
        let tiny = root.path().join("tiny");
        std::fs::create_dir(&big).unwrap();
        std::fs::create_dir(&tiny).unwrap();
        std::fs::write(big.join("a.bin"), vec![1u8; 2_000_000]).unwrap();
        std::fs::write(big.join("b.bin"), vec![2u8; 1_000_000]).unwrap();
        for index in 0..400 {
            std::fs::write(tiny.join(format!("{index}.txt")), vec![3u8; 4_000]).unwrap();
        }
        let tree = scanner::scan(scanner::ScanOptions::new(root.path())).unwrap();

        let rows = model::rows(&tree, tree.root(), None, true);
        let segments = layout(&tree, tree.root(), &rows);

        let ring_one: Vec<&Segment> = segments
            .iter()
            .filter(|segment| segment.ring == 1)
            .collect();
        assert_eq!(ring_one.first().unwrap().start, 0.0);
        assert!((ring_one.last().unwrap().end - 1.0).abs() < 1e-9);
        for pair in ring_one.windows(2) {
            assert!((pair[0].end - pair[1].start).abs() < 1e-12);
        }
        for segment in segments.iter().filter(|segment| segment.ring > 1) {
            let parent = &ring_one[segment.branch];
            assert!(segment.start >= parent.start - 1e-12 && segment.end <= parent.end + 1e-12);
        }
        let tiny_folder = tree
            .children(tree.root())
            .find(|&child| tree.name(child) == "tiny")
            .unwrap();
        let smaller = segments
            .iter()
            .find(|segment| segment.item == Item::Smaller(tiny_folder))
            .expect("tiny files are merged");
        assert_eq!(smaller.ring, 2);
        assert_eq!(smaller.size, tree.allocated(tiny_folder));
        assert!(
            segments
                .iter()
                .all(|segment| segment.end - segment.start >= MIN_FRACTION)
        );
    }

    /// `cargo test -p app --release -- --ignored --nocapture layout_time`
    #[test]
    #[ignore = "scans the home folder"]
    fn layout_time_on_the_home_folder() {
        let home = std::env::var_os("HOME").unwrap();
        let tree = scanner::scan(scanner::ScanOptions::new(&home)).unwrap();
        let mut folder = tree.root();
        for depth in 0..4 {
            let started = std::time::Instant::now();
            let rows = model::rows(&tree, folder, None, true);
            let segments = layout(&tree, folder, &rows);
            let elapsed = started.elapsed();
            println!(
                "depth {depth}: {} rows, {} slices, {:.2} ms",
                rows.len(),
                segments.len(),
                elapsed.as_secs_f64() * 1000.0
            );
            assert!(elapsed.as_millis() < 16, "layout must fit in a frame");
            let Some(next) = rows.iter().find_map(|row| match row.item {
                Item::Node(id) if model::is_folder(&tree, row.item) => Some(id),
                _ => None,
            }) else {
                break;
            };
            folder = next;
        }
    }

    #[test]
    fn screen_angles_run_clockwise_from_the_top() {
        let geometry = geometry();
        let top = geometry.point(0.0, 100.0);
        let right = geometry.point(0.25, 100.0);
        assert!((f32::from(top.y) - 116.0).abs() < 1e-3);
        assert!((f32::from(right.x) - 316.0).abs() < 1e-3);
    }
}
