//! The slide model and its evaluation at a time.
//!
//! A slide is a set of nodes, each a track of keyframes over states (a path and its paint, or
//! a placed picture), drawn in the order of the draw-order events:
//!
//! ```text
//!  node 0: ●──linear──●────────●──step──●        keyframe = (t, state or hidden, ease)
//!  node 1:      ●──────────linear──────●
//!  order:  [0]  [0, 1]                            back to front, from each event's t on
//! ```

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

/// Camera frame: pixel size and visible area in scene units (y up).
#[derive(Debug, Clone, Copy)]
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

/// Point index range `[start, end)` drawn as one subpath: 4 points per cubic curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Subpath {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) closed: bool,
}

/// Who sees a node: everyone, or only the lecturer (a cue).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seen {
    ByAll,
    ByLecturer,
}

/// A node's look: flat `[x, y, ...]` cubic control points and flat RGBA rows (several rows
/// are a gradient along `gradient`).
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
    pub(crate) seen: Seen,
}

/// A picture's pixels: straight RGBA, row by row from the top.
#[derive(Debug, Clone)]
pub(crate) struct Texture {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Arc<Vec<u8>>,
    /// The pixels as drawing wants them, made on first draw (see `render`).
    pub(crate) prepared: OnceLock<Arc<Vec<u8>>>,
}

/// A picture placed in the scene.
#[derive(Debug, Clone)]
pub(crate) struct ImageState {
    /// Index into the slide's textures.
    pub(crate) texture: usize,
    /// Pixel (x right, y down from the top left) to scene units, as `kurbo::Affine` takes it:
    /// `[a, b, c, d, e, f]` maps (x, y) to (a x + c y + e, b x + d y + f).
    pub(crate) transform: [f64; 6],
    /// Multiplies the pixels' own opacity.
    pub(crate) opacity: f64,
    pub(crate) seen: Seen,
}

/// What a node shows: a path or a picture.
#[derive(Debug, Clone)]
pub(crate) enum NodeState {
    Path(PathState),
    Image(ImageState),
}

impl NodeState {
    pub(crate) fn seen(&self) -> Seen {
        match self {
            NodeState::Path(path) => path.seen,
            NodeState::Image(image) => image.seen,
        }
    }
}

/// How a node reaches a keyframe from the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ease {
    Step,
    Linear,
}

#[derive(Debug, Clone)]
pub(crate) struct Keyframe {
    pub(crate) t: f64,
    /// Index into the slide's states; `None` hides the node from `t` on.
    pub(crate) state: Option<usize>,
    pub(crate) ease: Ease,
}

#[derive(Debug, Clone)]
pub(crate) struct Slide {
    pub(crate) kind: String,
    pub(crate) duration: f64,
    pub(crate) states: Vec<NodeState>,
    /// The pictures image states show.
    pub(crate) textures: Vec<Texture>,
    /// One keyframe list per node, sorted by time.
    pub(crate) tracks: Vec<Vec<Keyframe>>,
    /// `(t, nodes back to front)`, sorted by time.
    pub(crate) order: Vec<(f64, Vec<usize>)>,
    pub(crate) frame: Frame,
    /// `("#RRGGBB", opacity)`.
    pub(crate) background: (String, f64),
    /// Click stops inside the slide, increasing; they split it into parts.
    pub(crate) stops: Vec<f64>,
    /// Indices of the parts that repeat until the presenter goes on.
    pub(crate) loops: Vec<usize>,
}

impl Slide {
    /// Visible nodes at slide time `t`, back to front, with their states.
    pub(crate) fn evaluate(&self, t: f64) -> Vec<(usize, Cow<'_, NodeState>)> {
        let Some(event) = last_at_or_before(&self.order, |e| e.0, t) else {
            return vec![];
        };

        self.order[event]
            .1
            .iter()
            .filter_map(|&node| Some((node, self.state_at(node, t)?)))
            .collect()
    }

    /// The part playing at `t`: one more than the stops at or before it.
    ///
    /// Example: stops `[1, 2]` give part 0 for `t < 1`, part 1 for `1 <= t < 2`.
    pub(crate) fn part_at(&self, t: f64) -> usize {
        self.stops.partition_point(|&stop| stop <= t)
    }

    /// Start and end time of part `part`.
    pub(crate) fn part_range(&self, part: usize) -> (f64, f64) {
        let start = if part == 0 { 0.0 } else { self.stops[part - 1] };
        let end = self.stops.get(part).copied().unwrap_or(self.duration);
        (start, end)
    }

    pub(crate) fn part_loops(&self, part: usize) -> bool {
        self.loops.contains(&part)
    }

    /// The last part bound (start, stop or 0) before `t`, where going back halts.
    pub(crate) fn bound_before(&self, t: f64) -> f64 {
        // Stops increase, so the last one before `t` is the first from the end.
        self.stops
            .iter()
            .copied()
            .rev()
            .find(|&stop| stop < t)
            .unwrap_or(0.0)
    }

    /// State of one node at `t`, `None` while hidden.
    fn state_at(&self, node: usize, t: f64) -> Option<Cow<'_, NodeState>> {
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

        let alpha = (t - current.t) / (next.t - current.t);
        let blended = match (start, &self.states[target]) {
            (NodeState::Path(a), NodeState::Path(b)) => NodeState::Path(lerp_path(a, b, alpha)),
            (NodeState::Image(a), NodeState::Image(b)) => NodeState::Image(lerp_image(a, b, alpha)),
            _ => return Some(Cow::Borrowed(start)),
        };
        Some(Cow::Owned(blended))
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

/// Straight blend of two placements of the same picture.
fn lerp_image(a: &ImageState, b: &ImageState, alpha: f64) -> ImageState {
    ImageState {
        texture: a.texture,
        transform: std::array::from_fn(|i| mix_one(a.transform[i], b.transform[i], alpha)),
        opacity: mix_one(a.opacity, b.opacity, alpha),
        seen: a.seen,
    }
}

/// Straight blend of two states the converter found blendable (same shape structure).
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
        seen: a.seen,
    }
}
