//! What to draw for an evaluated state: path commands and resolved paint
//! (slide-format.md, section 10; reference: Manim's `fixtures._node`).

use super::timeline::{Cap, ImageState, Join, PathState, State};

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

/// One node to draw, in Manim units (y up).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DrawNode {
    /// Drawn as background stroke, then fill (nonzero rule), then stroke.
    Path {
        node: usize,
        subpaths: Vec<DrawSubpath>,
        background: Option<Line>,
        fill: Option<Paint>,
        stroke: Option<Line>,
        join: Join,
        cap: Cap,
    },
    Image {
        node: usize,
        image: usize,
        corners: [f64; 6],
        opacity: f64,
        smooth: bool,
    },
}

impl DrawNode {
    pub(crate) fn new(node: usize, state: &State) -> Self {
        match state {
            State::Path(path) => path_node(node, path),
            State::Image(image) => image_node(node, image),
        }
    }
}

fn path_node(node: usize, s: &PathState) -> DrawNode {
    let background = s
        .background
        .as_deref()
        .and_then(|rows| line(s.background_width, rows, s.gradient));

    DrawNode::Path {
        node,
        subpaths: subpaths(s),
        background,
        fill: paint(&s.fill, s.gradient),
        stroke: line(s.stroke_width, &s.stroke, s.gradient),
        join: s.join,
        cap: s.cap,
    }
}

fn image_node(node: usize, s: &ImageState) -> DrawNode {
    DrawNode::Image {
        node,
        image: s.image,
        corners: s.corners,
        opacity: s.opacity.clamp(0.0, 1.0),
        smooth: s.smooth,
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
