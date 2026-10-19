//! Reading a ManimGX take: its messages, coded arrays and uploads.
//!
//! The format is ManimGX's (`rust/engine/src/take.rs` and `pack.rs` there, version 1):
//!
//! ```text
//!  message  = len u32 | op u8 | fields            (little-endian; string = len u32 | bytes)
//!  START    = version u32 | width u32 | height u32 | fps f64
//!  ARRAY    = key u64 | base u64 | kind u8 | mode u8 | blob   (zstd, byte planes, delta to base)
//!  PATH     = key u64 | centroid_area f32×6 | points key | subpaths key
//!  GROW     = path key | closed u8 | tail key
//!  ROWS     = key u64 | rows key
//!  MESH     = key u64 | outline u32 | block u32 | points key | uvs key | normals key | triangles key
//!  TEXTURE  = key u64 | width u32 | height u32 | rgba key
//!  FRAME    = repeat u32 | view | records key | cameras…
//!  NOTE     = json
//! ```

use anyhow::{Context, anyhow, bail};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

/// The take format version this reader knows.
const VERSION: u32 = 1;

// Opcodes.
const START: u8 = 14;
const PATH: u8 = 1;
const POINTS: u8 = 2;
const MESH: u8 = 3;
const ROWS: u8 = 4;
const TEXTURE: u8 = 5;
const GROW: u8 = 6;
const FRAME: u8 = 8;
const NOTE: u8 = 9;
const SOUND: u8 = 10;
const ARRAY: u8 = 11;
const END: u8 = 12;
const ENVIRONMENT: u8 = 13;

// Array modes.
const ALONE: u8 = 0;
const FIRST: u8 = 1;
const SECOND: u8 = 2;
const TAIL: u8 = 3;

/// Bytes of one draw record.
pub(super) const RECORD: usize = 336;

/// An array kind: its element width in bytes, lanes, and whether the elements are floats.
#[derive(Debug, Clone, Copy)]
struct Kind {
    width: usize,
    lanes: usize,
    float: bool,
}

/// Kinds by their index in the ARRAY message (ManimGX `pack::Kind`).
const KINDS: [Kind; 10] = [
    Kind {
        width: 4,
        lanes: 3,
        float: true,
    }, // mesh points
    Kind {
        width: 4,
        lanes: 2,
        float: true,
    }, // mesh uvs
    Kind {
        width: 4,
        lanes: 3,
        float: true,
    }, // mesh normals
    Kind {
        width: 4,
        lanes: 1,
        float: false,
    }, // mesh triangles
    Kind {
        width: 8,
        lanes: 3,
        float: true,
    }, // path points (f64 xyz)
    Kind {
        width: 4,
        lanes: 4,
        float: false,
    }, // path subpaths
    Kind {
        width: 4,
        lanes: 4,
        float: true,
    }, // points
    Kind {
        width: 4,
        lanes: 4,
        float: true,
    }, // rows
    Kind {
        width: 1,
        lanes: 4,
        float: false,
    }, // texture
    Kind {
        width: 4,
        lanes: 84,
        float: false,
    }, // records
];

/// A path upload: cubic control points (x, y, z; 4 per curve) and subpaths
/// `[first curve, end curve, closed, 0]`.
#[derive(Debug, Clone)]
pub(super) struct Shape {
    pub(super) points: Arc<Vec<f64>>,
    pub(super) subpaths: Vec<[u32; 4]>,
    /// Curves appended later (`Create` of a growing path): from that frame on.
    pub(super) grows: Vec<Grow>,
}

#[derive(Debug, Clone)]
pub(super) struct Grow {
    /// The first frame that shows it.
    pub(super) frame: u64,
    pub(super) closed: bool,
    pub(super) tail: Arc<Vec<f64>>,
}

/// A mesh upload: vertices (x, y, z), texture coordinates (u, v) and triangles. A picture is a
/// textured quad.
#[derive(Debug, Clone)]
pub(super) struct Mesh {
    pub(super) points: Vec<f64>,
    pub(super) uvs: Vec<f64>,
    pub(super) triangles: Vec<u32>,
}

/// A texture upload: straight RGBA pixels, row by row from the top.
#[derive(Debug, Clone)]
pub(super) struct Texture {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rgba: Arc<Vec<u8>>,
}

/// Frames `[first, first + repeat)`, showing `records` through `view`.
#[derive(Debug, Clone)]
pub(super) struct Shot {
    pub(super) first: u64,
    pub(super) view: Vec<f32>,
    pub(super) records: Arc<Vec<u8>>,
}

/// One take, read.
#[derive(Debug, Default)]
pub(super) struct Take {
    pub(super) fps: f64,
    pub(super) frames: u64,
    pub(super) shots: Vec<Shot>,
    pub(super) shapes: HashMap<u64, Shape>,
    /// Brushes: straight sRGB RGBA rows.
    pub(super) rows: HashMap<u64, Arc<Vec<[f32; 4]>>>,
    pub(super) meshes: HashMap<u64, Mesh>,
    pub(super) textures: HashMap<u64, Texture>,
    /// Keys of uploads that are not drawn (points, environments): not converted.
    pub(super) other: HashMap<u64, u8>,
    pub(super) notes: Vec<serde_json::Value>,
}

/// A coded array as sent, decoded on first use.
#[derive(Debug)]
struct Coded {
    base: u64,
    kind: usize,
    mode: u8,
    blob: Vec<u8>,
}

/// A cursor over a message's fields.
struct Fields<'a> {
    bytes: &'a [u8],
}

impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> anyhow::Result<&'a [u8]> {
        if self.bytes.len() < n {
            bail!("message ends early");
        }
        let (head, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Ok(head)
    }

    fn u8(&mut self) -> anyhow::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> anyhow::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }

    fn u64(&mut self) -> anyhow::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }

    fn f64(&mut self) -> anyhow::Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into()?))
    }

    fn string(&mut self) -> anyhow::Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

/// The arrays of a take, decoded on demand and kept.
#[derive(Debug, Default)]
struct Arrays {
    coded: HashMap<u64, Coded>,
    decoded: HashMap<u64, Arc<Vec<u8>>>,
}

impl Arrays {
    /// Raw little-endian elements of array `key`.
    fn get(&mut self, key: u64) -> anyhow::Result<Arc<Vec<u8>>> {
        if let Some(bytes) = self.decoded.get(&key) {
            return Ok(Arc::clone(bytes));
        }
        let coded = self
            .coded
            .get(&key)
            .ok_or_else(|| anyhow!("unknown array {key:#x}"))?;
        let (base, kind, mode) = (coded.base, coded.kind, coded.mode);
        let values = unplane(&zstd(&coded.blob)?, KINDS[kind]);

        let bytes = match mode {
            ALONE => map_elements(&values, KINDS[kind], |v| unordered(v, KINDS[kind])),
            FIRST => {
                let base = self.get(base)?;
                if base.len() != values.len() * KINDS[kind].width {
                    bail!("array {key:#x} and its base differ in length");
                }
                zip_elements(&base, &values, KINDS[kind], |b, v| {
                    unordered(ordered(b, KINDS[kind]).wrapping_add(v), KINDS[kind])
                })
            }
            SECOND => {
                let older_key = self
                    .coded
                    .get(&base)
                    .map(|b| b.base)
                    .ok_or_else(|| anyhow!("base of {key:#x} unknown"))?;
                let base = self.get(base)?;
                let older = self.get(older_key)?;
                second(&base, &older, &values, KINDS[kind])?
            }
            TAIL => {
                let mut bytes = self.get(base)?.as_ref().clone();
                bytes.extend(map_elements(&values, KINDS[kind], |v| {
                    unordered(v, KINDS[kind])
                }));
                bytes
            }
            other => bail!("unknown array mode {other}"),
        };

        let bytes = Arc::new(bytes);
        self.decoded.insert(key, Arc::clone(&bytes));
        Ok(bytes)
    }

    fn f64s(&mut self, key: u64) -> anyhow::Result<Arc<Vec<f64>>> {
        let bytes = self.get(key)?;
        Ok(Arc::new(
            bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| f64::from_le_bytes(*c))
                .collect(),
        ))
    }

    fn f32s(&mut self, key: u64) -> anyhow::Result<Vec<f32>> {
        let bytes = self.get(key)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect())
    }

    fn u32s(&mut self, key: u64) -> anyhow::Result<Vec<u32>> {
        let bytes = self.get(key)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect())
    }
}

fn zstd(blob: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    ruzstd::decoding::StreamingDecoder::new(blob)
        .map_err(|e| anyhow!("zstd: {e}"))?
        .read_to_end(&mut out)
        .context("zstd")?;
    Ok(out)
}

/// Undo byte-plane and lane-major order: element `i`'s byte `b` sits at `b * n + at`.
fn unplane(bytes: &[u8], kind: Kind) -> Vec<u64> {
    let w = kind.width;
    let n = bytes.len() / w;
    let lanes = if kind.lanes > 1 && n.is_multiple_of(kind.lanes) {
        kind.lanes
    } else {
        1
    };
    let per = n / lanes.max(1);

    (0..n)
        .map(|i| {
            let at = (i % lanes) * per + i / lanes;
            (0..w).fold(0u64, |v, b| v | (bytes[b * n + at] as u64) << (8 * b))
        })
        .collect()
}

fn top(kind: Kind) -> u64 {
    1u64 << (8 * kind.width - 1)
}

fn mask(kind: Kind) -> u64 {
    if kind.width == 8 {
        u64::MAX
    } else {
        (1u64 << (8 * kind.width)) - 1
    }
}

/// Float bits mapped so integer order is float order (identity for integer kinds).
fn ordered(x: u64, kind: Kind) -> u64 {
    if !kind.float {
        return x;
    }
    let y = if x & top(kind) != 0 {
        !x
    } else {
        x | top(kind)
    };
    y & mask(kind)
}

fn unordered(y: u64, kind: Kind) -> u64 {
    if !kind.float {
        return y & mask(kind);
    }
    let x = if y & top(kind) != 0 {
        y ^ top(kind)
    } else {
        !y
    };
    x & mask(kind)
}

fn element(bytes: &[u8], i: usize, w: usize) -> u64 {
    (0..w).fold(0u64, |v, b| v | (bytes[i * w + b] as u64) << (8 * b))
}

fn map_elements(values: &[u64], kind: Kind, f: impl Fn(u64) -> u64) -> Vec<u8> {
    values
        .iter()
        .flat_map(|&v| f(v).to_le_bytes()[..kind.width].to_vec())
        .collect()
}

fn zip_elements(base: &[u8], values: &[u64], kind: Kind, f: impl Fn(u64, u64) -> u64) -> Vec<u8> {
    values
        .iter()
        .enumerate()
        .flat_map(|(i, &v)| f(element(base, i, kind.width), v).to_le_bytes()[..kind.width].to_vec())
        .collect()
}

/// Mode SECOND: a delta to the linear prediction `2 * base - older`.
fn second(base: &[u8], older: &[u8], values: &[u64], kind: Kind) -> anyhow::Result<Vec<u8>> {
    if !kind.float {
        bail!("second-order coding of an integer array");
    }
    if base.len() != values.len() * kind.width || older.len() != base.len() {
        bail!("second-order array and its bases differ in length");
    }

    let predict = |i: usize| -> u64 {
        let (b, o) = (element(base, i, kind.width), element(older, i, kind.width));
        if kind.width == 8 {
            (2.0 * f64::from_bits(b) - f64::from_bits(o)).to_bits()
        } else {
            (2.0 * f32::from_bits(b as u32) - f32::from_bits(o as u32)).to_bits() as u64
        }
    };
    Ok(values
        .iter()
        .enumerate()
        .flat_map(|(i, &v)| {
            let p = ordered(predict(i), kind).wrapping_add(v);
            unordered(p, kind).to_le_bytes()[..kind.width].to_vec()
        })
        .collect())
}

impl Take {
    /// Read a whole take: one START … END.
    pub(super) fn read(bytes: &[u8]) -> anyhow::Result<Self> {
        let mut take = Take::default();
        let mut arrays = Arrays::default();
        let mut started = false;
        let mut rest = bytes;

        while rest.len() >= 4 {
            let len = u32::from_le_bytes(rest[..4].try_into()?) as usize;
            let message = rest
                .get(4..4 + len)
                .ok_or_else(|| anyhow!("take ends inside a message"))?;
            rest = &rest[4 + len..];
            let (&op, fields) = message
                .split_first()
                .ok_or_else(|| anyhow!("empty message"))?;
            let mut f = Fields { bytes: fields };

            match op {
                START => {
                    let version = f.u32()?;
                    if version != VERSION {
                        bail!("take version {version}, this reader knows {VERSION}");
                    }
                    let (_width, _height) = (f.u32()?, f.u32()?);
                    take.fps = f.f64()?;
                    started = true;
                }
                ARRAY => {
                    let (key, base) = (f.u64()?, f.u64()?);
                    let (kind, mode) = (f.u8()? as usize, f.u8()?);
                    if kind >= KINDS.len() {
                        bail!("unknown array kind {kind}");
                    }
                    let blob = f.string()?.to_vec();
                    arrays.coded.insert(
                        key,
                        Coded {
                            base,
                            kind,
                            mode,
                            blob,
                        },
                    );
                }
                PATH => {
                    let key = f.u64()?;
                    let _centroid_area = f.take(24)?;
                    let (points, subpaths) = (f.u64()?, f.u64()?);
                    let subpaths = arrays.u32s(subpaths)?.as_chunks::<4>().0.to_vec();
                    take.shapes.insert(
                        key,
                        Shape {
                            points: arrays.f64s(points)?,
                            subpaths,
                            grows: vec![],
                        },
                    );
                }
                GROW => {
                    let (key, closed, tail) = (f.u64()?, f.u8()? != 0, f.u64()?);
                    let grow = Grow {
                        frame: take.frames,
                        closed,
                        tail: arrays.f64s(tail)?,
                    };
                    take.shapes
                        .get_mut(&key)
                        .ok_or_else(|| anyhow!("growing unknown path {key:#x}"))?
                        .grows
                        .push(grow);
                }
                ROWS => {
                    let (key, rows) = (f.u64()?, f.u64()?);
                    let rows = arrays.f32s(rows)?.as_chunks::<4>().0.to_vec();
                    take.rows.insert(key, Arc::new(rows));
                }
                MESH => {
                    let key = f.u64()?;
                    let (_outline, _block) = (f.u32()?, f.u32()?);
                    let (points, uvs, _normals, triangles) =
                        (f.u64()?, f.u64()?, f.u64()?, f.u64()?);
                    let mesh = Mesh {
                        points: arrays.f32s(points)?.into_iter().map(f64::from).collect(),
                        uvs: arrays.f32s(uvs)?.into_iter().map(f64::from).collect(),
                        triangles: arrays.u32s(triangles)?,
                    };
                    take.meshes.insert(key, mesh);
                }
                TEXTURE => {
                    let key = f.u64()?;
                    let (width, height) = (f.u32()?, f.u32()?);
                    let rgba = arrays.get(f.u64()?)?;
                    if rgba.len() != width as usize * height as usize * 4 {
                        bail!("texture {key:#x} is not {width} x {height} pixels");
                    }
                    take.textures.insert(
                        key,
                        Texture {
                            width,
                            height,
                            rgba,
                        },
                    );
                }
                POINTS | ENVIRONMENT => {
                    let key = if op == ENVIRONMENT {
                        f.u32()? as u64
                    } else {
                        f.u64()?
                    };
                    take.other.insert(key, op);
                }
                FRAME => {
                    let repeat = f.u32()? as u64;
                    let view = f
                        .string()?
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| f32::from_le_bytes(*c))
                        .collect();
                    let records = arrays.get(f.u64()?)?;
                    take.shots.push(Shot {
                        first: take.frames,
                        view,
                        records,
                    });
                    take.frames += repeat;
                }
                NOTE => take.notes.push(serde_json::from_slice(f.string()?)?),
                SOUND => {}
                END => {
                    if f.u8()? != 0 {
                        bail!("the scene failed while it was recorded");
                    }
                    break;
                }
                other => bail!("unknown take message {other}"),
            }
        }

        if !started || take.shots.is_empty() {
            bail!("not a complete take");
        }
        Ok(take)
    }
}
