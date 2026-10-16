//! What to draw for an evaluated state: path commands and resolved paint.

use super::timeline::{Cap, Join, PathState};

/// Numbers per RGBA row.
const RGBA_LEN: usize = 4;
const ALPHA: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Paint {
    Color([f64; 4]),
    /// Gradient line `[x1, y1, x2, y2]`, stops evenly spaced along it.
    Gradient {
        line: [f64; 4],
        stops: Vec<[f64; 4]>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Line {
    pub(crate) width: f64,
    pub(crate) paint: Paint,
}

/// `M move C cubic... [Z]`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DrawSubpath {
    pub(crate) start: [f64; 2],
    /// Two control points and the end point.
    pub(crate) cubics: Vec<[f64; 6]>,
    pub(crate) closed: bool,
}

/// One node to draw, in scene units (y up): background stroke, then fill (nonzero rule),
/// then stroke.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DrawPath {
    pub(crate) subpaths: Vec<DrawSubpath>,
    pub(crate) background: Option<Line>,
    pub(crate) fill: Option<Paint>,
    pub(crate) stroke: Option<Line>,
    pub(crate) join: Join,
    pub(crate) cap: Cap,
}

impl DrawPath {
    pub(crate) fn new(s: &PathState) -> Self {
        let background = s
            .background
            .as_deref()
            .and_then(|rows| line(s.background_width, rows, s.gradient));

        Self {
            subpaths: subpaths(s),
            background,
            fill: paint(&s.fill, s.gradient),
            stroke: line(s.stroke_width, &s.stroke, s.gradient),
            join: s.join,
            cap: s.cap,
        }
    }
}

/// One cubic per 4 points; its first point repeats the previous end.
/// Example: points 0..7 give `M p0 C p1 p2 p3 C p5 p6 p7`.
fn subpaths(s: &PathState) -> Vec<DrawSubpath> {
    let p = &s.points;
    let point = |i: usize| [p[2 * i], p[2 * i + 1]];

    s.subpaths
        .iter()
        .map(|sub| DrawSubpath {
            start: point(sub.start),
            cubics: (sub.start..sub.end.saturating_sub(3))
                .step_by(4)
                .map(|i| {
                    let [a, b, c] = [point(i + 1), point(i + 2), point(i + 3)];
                    [a[0], a[1], b[0], b[1], c[0], c[1]]
                })
                .collect(),
            closed: sub.closed,
        })
        .collect()
}

fn clamp_row(row: &[f64]) -> [f64; 4] {
    std::array::from_fn(|i| row[i].clamp(0.0, 1.0))
}

/// A gradient with several rows and a line, else the first row; `None` if transparent.
fn paint(rows: &[f64], gradient: Option<[f64; 4]>) -> Option<Paint> {
    if rows.len() > RGBA_LEN
        && let Some(line) = gradient
    {
        let stops = rows
            .as_chunks::<RGBA_LEN>()
            .0
            .iter()
            .map(|row| clamp_row(row))
            .collect();
        return Some(Paint::Gradient { line, stops });
    }

    if rows.len() < RGBA_LEN || rows[ALPHA] <= 0.0 {
        return None;
    }
    Some(Paint::Color(clamp_row(rows)))
}

fn line(width: f64, rows: &[f64], gradient: Option<[f64; 4]>) -> Option<Line> {
    if width <= 0.0 {
        return None;
    }
    Some(Line {
        width,
        paint: paint(rows, gradient)?,
    })
}
