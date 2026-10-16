//! The slide model and its evaluation at a time (slide-format.md, sections 7 and 8).

use super::ops;
use serde::Deserialize;
use std::borrow::Cow;

/// Camera frame: pixel size and visible area in Manim units (section 4.1).
#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct Frame {
    pub(crate) pw: f64,
    pub(crate) ph: f64,
    pub(crate) fw: f64,
    pub(crate) fh: f64,
    pub(crate) cx: f64,
    pub(crate) cy: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Join {
    Miter,
    Round,
    Bevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cap {
    Butt,
    Round,
    Square,
}

/// Point index range `[start, end)` drawn as one subpath (section 5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Subpath {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) closed: bool,
}

/// Flat `[x, y, ...]` points and RGBA rows, as in the file (section 5.1).
#[derive(Debug, Clone)]
pub(crate) struct PathState {
    pub(crate) points: Vec<f64>,
    pub(crate) subpaths: Vec<Subpath>,
    pub(crate) fill: Vec<f64>,
    pub(crate) stroke: Vec<f64>,
    pub(crate) stroke_width: f64,
    pub(crate) background: Option<Vec<f64>>,
    pub(crate) background_width: f64,
    pub(crate) gradient: Option<[f64; 4]>,
    pub(crate) join: Join,
    pub(crate) cap: Cap,
}

/// A bitmap stretched over three corners: upper left, upper right, lower left (section 5.3).
#[derive(Debug, Clone)]
pub(crate) struct ImageState {
    pub(crate) image: usize,
    pub(crate) corners: [f64; 6],
    pub(crate) opacity: f64,
    pub(crate) smooth: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum State {
    Path(PathState),
    Image(ImageState),
}

/// How a node reaches a keyframe from the previous one (section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ease {
    Step,
    Linear,
    Table(usize),
}

/// Motion replacing the straight blend of a segment (sections 7.3, 7.4).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Op {
    Arc {
        angle: f64,
        center: Option<[f64; 2]>,
    },
    Partial {
        full: usize,
        lower: Ease,
        upper: Ease,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Keyframe {
    pub(crate) t: f64,
    /// Index into the slide's states; `None` hides the node from `t` on.
    pub(crate) state: Option<usize>,
    pub(crate) ease: Ease,
    pub(crate) op: Option<Op>,
}

#[derive(Debug, Clone)]
pub(crate) struct Slide {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) duration: f64,
    pub(crate) states: Vec<State>,
    /// One keyframe list per node, sorted by time.
    pub(crate) tracks: Vec<Vec<Keyframe>>,
    /// `(t, nodes back to front)`, sorted by time.
    pub(crate) order: Vec<(f64, Vec<usize>)>,
    pub(crate) eases: Vec<Vec<f64>>,
    pub(crate) frame: Frame,
    /// `("#RRGGBB", opacity)`.
    pub(crate) background: (String, f64),
    /// `data:image/png;base64,...` URIs.
    pub(crate) images: Vec<String>,
}

impl Slide {
    /// Visible nodes at slide time `t`, back to front, with their states.
    pub(crate) fn evaluate(&self, t: f64) -> Vec<(usize, Cow<'_, State>)> {
        let Some(event) = last_at_or_before(&self.order, |e| e.0, t) else {
            return vec![];
        };

        self.order[event]
            .1
            .iter()
            .filter_map(|&node| Some((node, self.state_at(node, t)?)))
            .collect()
    }

    /// State of one node at `t`, `None` while hidden (section 7.1).
    fn state_at(&self, node: usize, t: f64) -> Option<Cow<'_, State>> {
        let track = self.tracks.get(node)?;
        let i = last_at_or_before(track, |k| k.t, t)?;
        let current = &track[i];
        let start = &self.states[current.state?];

        let Some(next) = track.get(i + 1) else {
            return Some(Cow::Borrowed(start));
        };
        let Some(target) = next.state else {
            return Some(Cow::Borrowed(start));
        };
        if next.ease == Ease::Step || next.t <= current.t {
            return Some(Cow::Borrowed(start));
        }

        let p = (t - current.t) / (next.t - current.t);
        Some(Cow::Owned(self.interpolate(start, target, next, p)))
    }

    fn interpolate(&self, start: &State, target: usize, next: &Keyframe, p: f64) -> State {
        // Partial: part of a full path, whatever the start state (section 7.4).
        if let Some(Op::Partial { full, lower, upper }) = next.op {
            let State::Path(full) = &self.states[full] else {
                return self.states[full].clone();
            };
            let points = ops::partial_points(
                &full.points,
                self.ease_value(lower, p),
                self.ease_value(upper, p),
            );
            let subpaths = ops::split_subpaths(&points);
            return State::Path(PathState {
                points,
                subpaths,
                ..full.clone()
            });
        }

        let alpha = self.ease_value(next.ease, p);
        let end = &self.states[target];
        let mut state = lerp_state(start, end, alpha);

        // Arc: points move along arcs, colors and widths still blend (section 7.3).
        if let Some(Op::Arc { angle, center }) = next.op {
            match (&mut state, start, end) {
                (State::Path(s), State::Path(a), State::Path(b)) => {
                    s.points = ops::arc_points(&a.points, &b.points, alpha, angle, center);
                }
                (State::Image(s), State::Image(a), State::Image(b)) => {
                    let q = ops::arc_points(&a.corners, &b.corners, alpha, angle, center);
                    s.corners.copy_from_slice(&q);
                }
                _ => {}
            }
        }
        state
    }

    /// Easing lookup with linear interpolation between samples (section 7.2).
    fn ease_value(&self, ease: Ease, p: f64) -> f64 {
        let Ease::Table(index) = ease else {
            return p;
        };
        let table = &self.eases[index];

        let x = p.clamp(0.0, 1.0) * (table.len() - 1) as f64;
        let i = (x.floor() as usize).min(table.len() - 2);
        table[i] + (table[i + 1] - table[i]) * (x - i as f64)
    }
}

/// Index of the last item whose time is `<= t` (the last of equal times).
fn last_at_or_before<T>(items: &[T], time: impl Fn(&T) -> f64, t: f64) -> Option<usize> {
    items.partition_point(|item| time(item) <= t).checked_sub(1)
}

fn mix(a: &[f64], b: &[f64], alpha: f64) -> Vec<f64> {
    a.iter().zip(b).map(|(a, b)| a + (b - a) * alpha).collect()
}

fn mix_one(a: f64, b: f64, alpha: f64) -> f64 {
    a + (b - a) * alpha
}

/// Straight blend; the writer guarantees compatible ends (section 7.1).
fn lerp_state(start: &State, end: &State, alpha: f64) -> State {
    match (start, end) {
        (State::Path(a), State::Path(b)) => State::Path(lerp_path(a, b, alpha)),
        (State::Image(a), State::Image(b)) => State::Image(ImageState {
            corners: std::array::from_fn(|i| mix_one(a.corners[i], b.corners[i], alpha)),
            opacity: mix_one(a.opacity, b.opacity, alpha),
            ..a.clone()
        }),
        _ => start.clone(),
    }
}

fn lerp_path(a: &PathState, b: &PathState, alpha: f64) -> PathState {
    // A missing background stroke blends as zeros of the other's length.
    let background = match (&a.background, &b.background) {
        (None, None) => None,
        (from, to) => {
            let len = from.as_ref().or(to.as_ref()).map_or(0, Vec::len);
            let zeros = vec![0.0; len];
            Some(mix(
                from.as_deref().unwrap_or(&zeros),
                to.as_deref().unwrap_or(&zeros),
                alpha,
            ))
        }
    };

    let gradient = match (a.gradient, b.gradient) {
        (Some(g), Some(h)) => Some(std::array::from_fn(|i| mix_one(g[i], h[i], alpha))),
        _ => None,
    };

    PathState {
        points: mix(&a.points, &b.points, alpha),
        subpaths: a.subpaths.clone(),
        fill: mix(&a.fill, &b.fill, alpha),
        stroke: mix(&a.stroke, &b.stroke, alpha),
        stroke_width: mix_one(a.stroke_width, b.stroke_width, alpha),
        background,
        background_width: mix_one(a.background_width, b.background_width, alpha),
        gradient,
        join: a.join,
        cap: a.cap,
    }
}
