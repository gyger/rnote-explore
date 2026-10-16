//! A take's shots as a slide: each record a state per shot, one node per record slot.
//!
//! ```text
//!  shot (frames [first, first + repeat)) ──► for each record, in draw order:
//!     shape key1 (+ grows) ──M1──► scene points (+ M2·shape key2 for a morph)
//!     reveal window [lo, hi] ──► curves cut by de Casteljau
//!     paint (+ paint 2 by mix) ──► fill / stroke / background rows, mixed in OKLab
//!  ──► keyframe of node = slot at t = first / fps
//! ```
//!
//! Consecutive states of a slot blend linearly when their shape is the same (a move, a fade),
//! and step otherwise. Records that are not paths (meshes, points, images) are left out.

use super::read::{RECORD, Shape, Shot, Take};
use crate::manimslide::ops;
use crate::manimslide::timeline::{
    Cap, Ease, Frame, Join, Keyframe, PathState, Seen, Slide, Subpath,
};
use anyhow::bail;
use std::collections::{HashMap, HashSet};

// Record layout: 8 u64 keys, then 68 f32 (ManimGX `render::Record`).
const KEY1: usize = 0;
const KEY2: usize = 1;
const FILL_ROWS: usize = 2;
const STROKE_ROWS: usize = 3;
const FILL_ROWS2: usize = 4;
const STROKE_ROWS2: usize = 5;
const FLAGS: usize = 7;
/// Float offsets, counted from the first float (byte 64).
const M1: usize = 0;
const M2: usize = 12;
const FILL: usize = 24;
const STROKE: usize = 28;
const BACKGROUND: usize = 32;
const FILL2: usize = 36;
const STROKE2: usize = 40;
const BACKGROUND2: usize = 44;
const PARAMS: usize = 48;
const GRADIENT_A: usize = 52;
const GRADIENT_B: usize = 56;
const DASH: usize = 60;
const FLOATS_AT: usize = 64;

const FLAG_OVERLAY: u64 = 1;
/// A cue, for the lecturer only: set by Rnote's driver (`record_takes.py`), not by ManimGX.
const FLAG_CUE: u64 = 1 << 32;
const CAP_SHIFT: u64 = 6;
const JOINT_SHIFT: u64 = 8;
const TWO_BITS: u64 = 3;

// View layout (f32).
const VIEW_PROJECTION: usize = 0;
const VIEW_WIDTH: usize = 32;
const VIEW_HEIGHT: usize = 33;
const VIEW_THREE_D: usize = 39;
const VIEW_BACKGROUND: usize = 44;
const VIEW_MIN_LEN: usize = 48;

/// Section types (ManimGX `next_section`).
const SECTION_SKIP: &str = "presentation.skip";
const SECTION_LOOPS: [&str; 2] = ["presentation.loop", "presentation.complete_loop"];
/// The slide type when the last section goes on by itself (slide-format.md 9).
const TYPE_SKIP: &str = "presentation.skip";
const TYPE_NORMAL: &str = "default.normal";

/// Numbers per point in a take (x, y, z) and in a slide (x, y).
const TAKE_DIMS: usize = 3;

struct Record<'a> {
    bytes: &'a [u8],
}

impl Record<'_> {
    fn key(&self, i: usize) -> u64 {
        u64::from_le_bytes(self.bytes[8 * i..8 * i + 8].try_into().unwrap())
    }

    fn f(&self, i: usize) -> f64 {
        let at = FLOATS_AT + 4 * i;
        f32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap()) as f64
    }

    fn rgba(&self, i: usize) -> [f64; 4] {
        std::array::from_fn(|k| self.f(i + k))
    }

    fn matrix(&self, i: usize) -> [f64; 12] {
        std::array::from_fn(|k| self.f(i + k))
    }

    fn overlay(&self) -> bool {
        self.key(FLAGS) & FLAG_OVERLAY != 0
    }

    fn seen(&self) -> Seen {
        if self.key(FLAGS) & FLAG_CUE != 0 {
            return Seen::ByLecturer;
        }
        Seen::ByAll
    }
}

/// The camera of a 2D view: frame size and center in scene units.
fn frame_of(shot: &Shot) -> anyhow::Result<Frame> {
    let v = &shot.view;
    if v.len() < VIEW_MIN_LEN {
        bail!("view too short");
    }
    if v[VIEW_THREE_D] > 0.5 {
        bail!("3D scenes cannot be shown as 2D slides");
    }

    // Projection, column-major: clip_x = (x - cx) / hw, so P00 = 1/hw and P03 = -cx/hw.
    let p = |r: usize, c: usize| v[VIEW_PROJECTION + c * 4 + r] as f64;
    let (hw, hh) = (1.0 / p(0, 0), 1.0 / p(1, 1));
    Ok(Frame {
        pw: v[VIEW_WIDTH] as f64,
        ph: v[VIEW_HEIGHT] as f64,
        fw: 2.0 * hw,
        fh: 2.0 * hh,
        cx: -p(0, 3) * hw,
        cy: -p(1, 3) * hh,
    })
}

fn background_of(shot: &Shot) -> (String, f64) {
    let c = |i: usize| (shot.view[VIEW_BACKGROUND + i].clamp(0.0, 1.0) * 255.0).round() as u8;
    (
        format!("#{:02X}{:02X}{:02X}", c(0), c(1), c(2)),
        shot.view[VIEW_BACKGROUND + 3] as f64,
    )
}

// -- color: ManimGX mixes in OKLab, weighted by opacity (render.rs mix_rgba) ---------

fn to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn oklab(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(to_linear);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn from_oklab([lab_l, a, b]: [f64; 3]) -> [f64; 3] {
    let l = (lab_l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (lab_l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (lab_l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
    .map(to_srgb)
}

/// ManimGX's paint mix: OKLab weighted by each side's opacity.
fn mix_rgba(a: [f64; 4], b: [f64; 4], t: f64) -> [f64; 4] {
    if t <= 0.0 || a == b {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let (wa, wb) = (a[3] * (1.0 - t), b[3] * t);
    let opacity = wa + wb;
    if a[..3] == b[..3] {
        return [a[0], a[1], a[2], opacity];
    }

    let (la, lb) = (oklab([a[0], a[1], a[2]]), oklab([b[0], b[1], b[2]]));
    let lab: [f64; 3] = if opacity > 1e-9 {
        std::array::from_fn(|i| (la[i] * wa + lb[i] * wb) / opacity)
    } else {
        std::array::from_fn(|i| la[i] + (lb[i] - la[i]) * t)
    };
    let [r, g, bl] = from_oklab(lab);
    [r, g, bl, opacity]
}

/// Flat RGBA rows of one paint: the brush's rows (mixed with paint 2's) or its one color.
fn paint_rows(
    take: &Take,
    rec: &Record,
    color: usize,
    color2: usize,
    rows: usize,
    rows2: usize,
) -> Vec<f64> {
    let mix = rec.f(DASH + 3);
    let brush = take.rows.get(&rec.key(rows)).filter(|r| r.len() > 1);
    let Some(brush) = brush else {
        return mix_rgba(rec.rgba(color), rec.rgba(color2), mix).to_vec();
    };

    let brush2 = take
        .rows
        .get(&rec.key(rows2))
        .filter(|r| r.len() == brush.len())
        .unwrap_or(brush);
    brush
        .iter()
        .zip(brush2.iter())
        .flat_map(|(a, b)| mix_rgba(a.map(f64::from), b.map(f64::from), mix))
        .collect()
}

// -- geometry ------------------------------------------------------------------------

/// The shape's control points with the curves grown by frame `frame`, and its subpaths.
fn grown(shape: &Shape, frame: u64) -> (Vec<f64>, Vec<[u32; 4]>) {
    let mut points = shape.points.as_ref().clone();
    let mut subpaths = shape.subpaths.clone();
    for grow in shape.grows.iter().filter(|g| g.frame <= frame) {
        let added = (grow.tail.len() / (TAKE_DIMS * 4)) as u32;
        points.extend_from_slice(&grow.tail);
        if let Some(last) = subpaths.last_mut() {
            last[1] += added;
            last[2] = grow.closed as u32;
        }
    }
    (points, subpaths)
}

/// Scene x, y of every control point: `M1·p1 (+ M2·p2)`, plus `offset`.
fn place(
    p1: &[f64],
    m1: [f64; 12],
    term2: Option<(&[f64], [f64; 12])>,
    offset: [f64; 2],
) -> Vec<f64> {
    let apply = |m: &[f64; 12], p: &[f64]| -> [f64; 2] {
        [
            m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + m[3],
            m[4] * p[0] + m[5] * p[1] + m[6] * p[2] + m[7],
        ]
    };

    p1.as_chunks::<TAKE_DIMS>()
        .0
        .iter()
        .enumerate()
        .flat_map(|(j, p)| {
            let mut xy = apply(&m1, p);
            if let Some((p2, m2)) = term2 {
                let q = apply(&m2, &p2[j * TAKE_DIMS..j * TAKE_DIMS + TAKE_DIMS]);
                xy = [xy[0] + q[0], xy[1] + q[1]];
            }
            [xy[0] + offset[0], xy[1] + offset[1]]
        })
        .collect()
}

/// The part of each subpath inside the reveal window `[lo, hi]` (in curve index + t).
fn reveal(points: &[f64], subpaths: &[[u32; 4]], lo: f64, hi: f64) -> (Vec<f64>, Vec<Subpath>) {
    const CURVE: usize = 8;
    let mut out = Vec::with_capacity(points.len());
    let mut out_subpaths = Vec::with_capacity(subpaths.len());

    for &[first, end, closed, _] in subpaths {
        let start_point = out.len() / 2;
        for c in first..end {
            let curve = &points[CURVE * c as usize..CURVE * (c as usize + 1)];
            let (a, b) = (
                (lo - c as f64).clamp(0.0, 1.0),
                (hi - c as f64).clamp(0.0, 1.0),
            );
            if b <= a {
                continue;
            }
            out.extend(ops::partial_points(curve, a, b));
        }

        let end_point = out.len() / 2;
        if end_point > start_point {
            out_subpaths.push(Subpath {
                start: start_point,
                end: end_point,
                closed: closed != 0 && lo <= first as f64 && hi >= end as f64,
            });
        }
    }
    (out, out_subpaths)
}

fn cap_of(flags: u64) -> Cap {
    match (flags >> CAP_SHIFT) & TWO_BITS {
        1 => Cap::Round,
        2 => Cap::Square,
        _ => Cap::Butt,
    }
}

fn join_of(flags: u64) -> Join {
    match (flags >> JOINT_SHIFT) & TWO_BITS {
        1 => Join::Round,
        2 => Join::Bevel,
        _ => Join::Miter,
    }
}

/// The state a path record shows; `None` for records that are not paths.
fn state_of(take: &Take, rec: &Record, frame: u64, center: [f64; 2]) -> Option<PathState> {
    let shape = take.shapes.get(&rec.key(KEY1))?;
    let (p1, subpaths) = grown(shape, frame);

    let p2 = take.shapes.get(&rec.key(KEY2)).map(|s| grown(s, frame).0);
    let term2 = p2
        .as_deref()
        .filter(|p2| p2.len() == p1.len())
        .map(|p2| (p2, rec.matrix(M2)));
    // Records fixed in the frame are placed around its center.
    let offset = if rec.overlay() { center } else { [0.0, 0.0] };
    let placed = place(&p1, rec.matrix(M1), term2, offset);

    let (lo, hi) = (rec.f(PARAMS), rec.f(PARAMS + 1));
    let (points, subpaths) = reveal(&placed, &subpaths, lo, hi);

    let fill = paint_rows(take, rec, FILL, FILL2, FILL_ROWS, FILL_ROWS2);
    let stroke = paint_rows(take, rec, STROKE, STROKE2, STROKE_ROWS, STROKE_ROWS2);
    let background = mix_rgba(rec.rgba(BACKGROUND), rec.rgba(BACKGROUND2), rec.f(DASH + 3));
    let background_width = rec.f(PARAMS + 3);
    let gradient = (fill.len() > 4 || stroke.len() > 4).then(|| {
        [
            rec.f(GRADIENT_A) + offset[0],
            rec.f(GRADIENT_A + 1) + offset[1],
            rec.f(GRADIENT_B) + offset[0],
            rec.f(GRADIENT_B + 1) + offset[1],
        ]
    });

    Some(PathState {
        points,
        subpaths,
        fill,
        stroke,
        stroke_width: rec.f(PARAMS + 2),
        background: (background_width > 0.0 && background[3] > 0.0).then(|| background.to_vec()),
        background_width,
        gradient,
        join: join_of(rec.key(FLAGS)),
        cap: cap_of(rec.key(FLAGS)),
        seen: rec.seen(),
    })
}

/// Whether a slot can blend from `a` to `b`: the same shape, just moved or recolored.
fn blendable(a: &PathState, b: &PathState) -> bool {
    a.points.len() == b.points.len()
        && a.subpaths == b.subpaths
        && a.fill.len() == b.fill.len()
        && a.stroke.len() == b.stroke.len()
        && a.background.as_ref().map(Vec::len) == b.background.as_ref().map(Vec::len)
        && a.gradient.is_some() == b.gradient.is_some()
        && a.join == b.join
        && a.cap == b.cap
        && a.seen == b.seen
}

/// States, deduplicated by their exact numbers.
#[derive(Default)]
struct States {
    states: Vec<PathState>,
    index: HashMap<Vec<u64>, usize>,
}

impl States {
    fn add(&mut self, state: PathState) -> usize {
        let key: Vec<u64> = state
            .points
            .iter()
            .chain(&state.fill)
            .chain(&state.stroke)
            .chain(state.background.iter().flatten())
            .chain(state.gradient.iter().flatten())
            .chain([state.stroke_width, state.background_width].iter())
            .map(|v| v.to_bits())
            .chain(
                state
                    .subpaths
                    .iter()
                    .flat_map(|s| [s.start as u64, s.end as u64, s.closed as u64]),
            )
            .chain([
                state.join as u64,
                state.cap as u64,
                state.seen as u64,
                state.points.len() as u64,
            ])
            .collect();
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        self.states.push(state);
        self.index.insert(key, self.states.len() - 1);
        self.states.len() - 1
    }

    fn path(&self, i: usize) -> &PathState {
        &self.states[i]
    }
}

/// Drop keyframes that change nothing: same state as the one before, unless motion starts there.
fn compress(track: Vec<Keyframe>) -> Vec<Keyframe> {
    let mut out: Vec<Keyframe> = Vec::with_capacity(track.len());
    for (i, keyframe) in track.iter().enumerate() {
        if let Some(previous) = out.last()
            && previous.state == keyframe.state
        {
            let next = track.get(i + 1);
            let starts_motion =
                next.is_some_and(|n| n.ease != Ease::Step && n.state != keyframe.state);
            if !starts_motion {
                continue;
            }
        }
        out.push(keyframe.clone());
    }
    out
}

/// Sections as parts: stops where a section begins after one that does not go on by itself,
/// looped parts for the looping types.
fn parts(take: &Take) -> (Vec<f64>, Vec<usize>, &'static str) {
    let mut sections: Vec<(u64, String)> = take
        .notes
        .iter()
        .filter_map(|note| {
            let s = note.get("section")?;
            Some((
                s.get("frame")?.as_u64()?,
                s.get("type")?.as_str()?.to_owned(),
            ))
        })
        .collect();
    sections.sort_by_key(|(frame, _)| *frame);
    if sections.first().is_none_or(|(frame, _)| *frame > 0) {
        sections.insert(0, (0, TYPE_NORMAL.to_owned()));
    }
    // A section without frames is replaced by the next one.
    sections.dedup_by(|later, earlier| {
        let empty = later.0 == earlier.0;
        if empty {
            *earlier = later.clone();
        }
        empty
    });
    sections.retain(|(frame, _)| *frame < take.frames);

    let mut stops = Vec::new();
    let mut loops = Vec::new();
    for (i, (frame, kind)) in sections.iter().enumerate() {
        let looped = SECTION_LOOPS.contains(&kind.as_str());
        if i > 0 && (sections[i - 1].1 != SECTION_SKIP || looped) {
            stops.push(*frame as f64 / take.fps);
        }
        if looped {
            loops.push(stops.len());
        }
    }
    let kind = if sections.last().is_some_and(|(_, k)| k == SECTION_SKIP) {
        TYPE_SKIP
    } else {
        TYPE_NORMAL
    };
    (stops, loops, kind)
}

pub(super) fn slide_of(take: &Take) -> anyhow::Result<Slide> {
    let first = take.shots.first().expect("a take has shots");
    let frame = frame_of(first)?;
    let center = [frame.cx, frame.cy];

    let mut states = States::default();
    let mut tracks: Vec<Vec<Keyframe>> = Vec::new();
    let mut keys: Vec<(u64, u64)> = Vec::new();
    let mut order: Vec<(f64, Vec<usize>)> = Vec::new();

    for shot in &take.shots {
        let t = shot.first as f64 / take.fps;

        // Draw order: the records' order, those fixed in the frame last.
        let mut records: Vec<Record> = shot
            .records
            .as_chunks::<RECORD>()
            .0
            .iter()
            .map(|bytes| Record { bytes })
            .collect();
        records.sort_by_key(Record::overlay);

        let mut shown = Vec::with_capacity(records.len());
        for (slot, rec) in records.iter().enumerate() {
            let Some(state) = state_of(take, rec, shot.first, center) else {
                continue;
            };
            if tracks.len() <= slot {
                tracks.resize_with(slot + 1, Vec::new);
                keys.resize(slot + 1, (0, 0));
            }

            let index = states.add(state);
            let shape_keys = (rec.key(KEY1), rec.key(KEY2));
            let track = &mut tracks[slot];
            let ease = match track.last().and_then(|k| k.state) {
                Some(previous)
                    if keys[slot] == shape_keys
                        && blendable(states.path(previous), states.path(index)) =>
                {
                    Ease::Linear
                }
                _ => Ease::Step,
            };
            track.push(Keyframe {
                t,
                state: Some(index),
                ease,
            });
            keys[slot] = shape_keys;
            shown.push(slot);
        }

        // Slots not shown now are hidden.
        let shown_set: HashSet<usize> = shown.iter().copied().collect();
        for (slot, track) in tracks.iter_mut().enumerate() {
            if !shown_set.contains(&slot) && track.last().is_some_and(|k| k.state.is_some()) {
                track.push(Keyframe {
                    t,
                    state: None,
                    ease: Ease::Step,
                });
            }
        }
        if order.last().is_none_or(|(_, last)| *last != shown) {
            order.push((t, shown));
        }
    }

    let (stops, loops, kind) = parts(take);
    Ok(Slide {
        kind: kind.to_owned(),
        duration: take.frames as f64 / take.fps,
        states: states.states,
        tracks: tracks.into_iter().map(compress).collect(),
        order,
        frame,
        background: background_of(first),
        stops,
        loops,
    })
}
