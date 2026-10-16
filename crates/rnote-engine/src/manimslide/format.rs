//! Slide file decoding (slide-format.md, sections 2, 3, 5 and 7).

use super::timeline::{
    Cap, Ease, Frame, ImageState, Join, Keyframe, Op, PathState, Slide, State, Subpath,
};
use anyhow::{Context, anyhow, bail};
use base64::Engine as _;
use serde::Deserialize;
use std::io::Read;

/// The only format version this reader knows.
const FORMAT_VERSION: u64 = 1;
/// Packed coordinates are integers in units of 1 / SCALE (section 2.2).
const SCALE: f64 = 10_000.0;
/// Keyframe ease values with a meaning of their own (section 7).
const EASE_STEP: i64 = -1;
const EASE_LINEAR: i64 = -2;
/// Keyframe state index of a hidden node.
const STATE_HIDDEN: i64 = -1;

#[derive(Debug, Deserialize)]
struct RawSlide {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    duration: f64,
    states: Vec<RawState>,
    tracks: Vec<Vec<RawKeyframe>>,
    order: Vec<(f64, Vec<usize>)>,
    eases: Vec<Vec<f64>>,
    frame: Frame,
    background: (String, f64),
    #[serde(default)]
    images: Vec<String>,
}

/// A path or image state, packed (`pd`, `pr`) or not (`p`).
#[derive(Debug, Deserialize)]
struct RawState {
    p: Option<Vec<f64>>,
    pd: Option<Vec<i64>>,
    pr: Option<usize>,
    s: Option<Vec<usize>>,
    f: Option<Vec<f64>>,
    st: Option<Vec<f64>>,
    sw: Option<f64>,
    bs: Option<Vec<f64>>,
    bw: Option<f64>,
    g: Option<[f64; 4]>,
    j: Option<String>,
    c: Option<String>,
    img: Option<usize>,
    q: Option<[f64; 6]>,
    o: Option<f64>,
    n: Option<u8>,
}

/// `[t, state, ease]` or `[t, state, ease, op]`.
#[derive(Debug, Deserialize)]
struct RawKeyframe(f64, i64, i64, #[serde(default)] Option<RawOp>);

#[derive(Debug, Deserialize)]
struct RawOp {
    arc: Option<f64>,
    c: Option<[f64; 2]>,
    part: Option<usize>,
    lo: Option<i64>,
    hi: Option<i64>,
}

impl Slide {
    /// Read a slide file, packed (as Manim writes it) or unpacked.
    pub(crate) fn from_json(text: &str) -> anyhow::Result<Self> {
        let mut value: serde_json::Value =
            serde_json::from_str(text).context("slide file is not JSON")?;

        let version = value.get("version").and_then(|v| v.as_u64());
        if version != Some(FORMAT_VERSION) {
            bail!("unsupported slide format version {version:?}");
        }

        // Packed: everything but `version` and `images` sits deflated in `z`.
        if let Some(blob) = value.get("z").and_then(|z| z.as_str()) {
            let images = value.get("images").cloned().unwrap_or_default();
            value = inflate(blob)?;
            value["images"] = images;
        }

        let raw: RawSlide = serde_json::from_value(value).context("malformed slide object")?;
        raw.into_slide()
    }
}

/// Base64, then zlib (RFC 1950), then JSON (section 2.1).
fn inflate(blob: &str) -> anyhow::Result<serde_json::Value> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .context("packed slide is not base64")?;
    let mut text = String::new();
    flate2::read::ZlibDecoder::new(bytes.as_slice())
        .read_to_string(&mut text)
        .context("packed slide does not inflate")?;
    serde_json::from_str(&text).context("packed slide is not JSON")
}

impl RawSlide {
    fn into_slide(self) -> anyhow::Result<Slide> {
        let states = decode_states(self.states)?;
        let tracks = self
            .tracks
            .into_iter()
            .map(|track| track.into_iter().map(keyframe).collect())
            .collect::<anyhow::Result<_>>()?;

        Ok(Slide {
            name: self.name,
            kind: self.kind,
            duration: self.duration,
            states,
            tracks,
            order: self.order,
            eases: self.eases,
            frame: self.frame,
            background: self.background,
            images: self.images,
        })
    }
}

/// Decode states in index order; `pr` refers to an earlier state's integers.
fn decode_states(raw: Vec<RawState>) -> anyhow::Result<Vec<State>> {
    let mut ints: Vec<Option<Vec<i64>>> = Vec::with_capacity(raw.len());
    let mut states = Vec::with_capacity(raw.len());

    for (index, state) in raw.into_iter().enumerate() {
        let q = packed_points(&state, &ints).with_context(|| format!("state {index}"))?;
        let points = match (&q, &state.p) {
            (Some(q), _) => Some(q.iter().map(|&v| v as f64 / SCALE).collect()),
            (None, p) => p.clone(),
        };
        ints.push(q);
        states.push(state_of(state, points).with_context(|| format!("state {index}"))?);
    }
    Ok(states)
}

/// Integer coordinates of a packed path state, `None` when not packed.
///
/// Example: `pd = [10, 20, 1, 2]` without `pr` gives `[10, 20, 11, 22]`.
fn packed_points(state: &RawState, ints: &[Option<Vec<i64>>]) -> anyhow::Result<Option<Vec<i64>>> {
    let Some(d) = &state.pd else {
        return Ok(None);
    };

    if let Some(reference) = state.pr {
        let base = ints
            .get(reference)
            .and_then(|b| b.as_ref())
            .ok_or_else(|| anyhow!("pr {reference} is not an earlier path state"))?;
        if base.len() != d.len() {
            bail!("pr {reference} has another length");
        }
        return Ok(Some(base.iter().zip(d).map(|(b, d)| b + d).collect()));
    }

    let mut q = d.clone();
    for k in 2..q.len() {
        q[k] += q[k - 2];
    }
    Ok(Some(q))
}

fn state_of(raw: RawState, points: Option<Vec<f64>>) -> anyhow::Result<State> {
    if let Some(image) = raw.img {
        return Ok(State::Image(ImageState {
            image,
            corners: raw.q.ok_or_else(|| anyhow!("image state without q"))?,
            opacity: raw.o.ok_or_else(|| anyhow!("image state without o"))?,
            smooth: raw.n != Some(1),
        }));
    }

    let s = raw.s.ok_or_else(|| anyhow!("path state without s"))?;
    if s.len() % 3 != 0 {
        bail!("s is not made of triples");
    }
    let (triples, _) = s.as_chunks::<3>();
    let subpaths = triples
        .iter()
        .map(|&[start, end, closed]| Subpath {
            start,
            end,
            closed: closed != 0,
        })
        .collect();

    Ok(State::Path(PathState {
        points: points.ok_or_else(|| anyhow!("path state without points"))?,
        subpaths,
        fill: raw.f.ok_or_else(|| anyhow!("path state without f"))?,
        stroke: raw.st.ok_or_else(|| anyhow!("path state without st"))?,
        stroke_width: raw.sw.ok_or_else(|| anyhow!("path state without sw"))?,
        background: raw.bs,
        background_width: raw.bw.unwrap_or(0.0),
        gradient: raw.g,
        join: join(raw.j.as_deref())?,
        cap: cap(raw.c.as_deref())?,
    }))
}

fn join(name: Option<&str>) -> anyhow::Result<Join> {
    match name {
        None | Some("miter") => Ok(Join::Miter),
        Some("round") => Ok(Join::Round),
        Some("bevel") => Ok(Join::Bevel),
        Some(other) => bail!("unknown line join {other}"),
    }
}

fn cap(name: Option<&str>) -> anyhow::Result<Cap> {
    match name {
        None | Some("butt") => Ok(Cap::Butt),
        Some("round") => Ok(Cap::Round),
        Some("square") => Ok(Cap::Square),
        Some(other) => bail!("unknown line cap {other}"),
    }
}

fn ease(value: i64) -> anyhow::Result<Ease> {
    match value {
        EASE_STEP => Ok(Ease::Step),
        EASE_LINEAR => Ok(Ease::Linear),
        k if k >= 0 => Ok(Ease::Table(k as usize)),
        other => bail!("unknown ease {other}"),
    }
}

fn keyframe(raw: RawKeyframe) -> anyhow::Result<Keyframe> {
    let RawKeyframe(t, state, ease_value, op) = raw;
    let state = match state {
        STATE_HIDDEN => None,
        index if index >= 0 => Some(index as usize),
        other => bail!("unknown state index {other}"),
    };

    Ok(Keyframe {
        t,
        state,
        ease: ease(ease_value)?,
        op: op.map(operation).transpose()?,
    })
}

fn operation(raw: RawOp) -> anyhow::Result<Op> {
    if let Some(angle) = raw.arc {
        return Ok(Op::Arc {
            angle,
            center: raw.c,
        });
    }

    let full = raw
        .part
        .ok_or_else(|| anyhow!("op is neither arc nor part"))?;
    Ok(Op::Partial {
        full,
        lower: ease(raw.lo.ok_or_else(|| anyhow!("part op without lo"))?)?,
        upper: ease(raw.hi.ok_or_else(|| anyhow!("part op without hi"))?)?,
    })
}
