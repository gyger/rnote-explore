//! Drawing an evaluated slide with piet (slide-format.md, section 10).
//!
//! ```text
//!  target rect (document)  ◄── scale + center ──  pixel frame (pw × ph)  ◄── frame matrix ──  Manim units (y up)
//! ```

use super::draw::{DrawNode, DrawSubpath, Line, Paint};
use super::timeline::{Cap, Frame, Join, Slide};
use anyhow::{Context, anyhow};
use base64::Engine as _;
use kurbo::{Affine, BezPath, Rect};
use piet::{RenderContext, StrokeStyle};

const DATA_URI_PREFIX: &str = "data:image/png;base64,";
/// Cairo's default, which Manim draws with (section 10.1).
const MITER_LIMIT: f64 = 10.0;
/// Bytes per RGBA pixel.
const RGBA_BYTES: usize = 4;

/// A slide's images, decoded once to straight-alpha RGBA.
#[derive(Debug, Clone)]
pub(crate) struct SlideImages(Vec<Bitmap>);

#[derive(Debug, Clone)]
struct Bitmap {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl SlideImages {
    pub(crate) fn decode(slide: &Slide) -> anyhow::Result<Self> {
        slide
            .images
            .iter()
            .enumerate()
            .map(|(i, uri)| decode_png_uri(uri).with_context(|| format!("image {i}")))
            .collect::<anyhow::Result<_>>()
            .map(Self)
    }
}

fn decode_png_uri(uri: &str) -> anyhow::Result<Bitmap> {
    let data = uri
        .strip_prefix(DATA_URI_PREFIX)
        .ok_or_else(|| anyhow!("not a PNG data URI"))?;
    let png = base64::engine::general_purpose::STANDARD.decode(data)?;
    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?.into_rgba8();

    Ok(Bitmap {
        width: image.width() as usize,
        height: image.height() as usize,
        rgba: image.into_raw(),
    })
}

impl Slide {
    /// Draw the slide at time `t` into `target`, centered with its aspect ratio kept.
    pub(crate) fn draw_at(
        &self,
        cx: &mut impl RenderContext,
        images: &SlideImages,
        t: f64,
        target: Rect,
    ) -> anyhow::Result<()> {
        let to_target = fit(&self.frame, target);
        let to_pixels = frame_matrix(&self.frame);
        let pixels = Rect::new(0.0, 0.0, self.frame.pw, self.frame.ph);

        cx.with_save(|cx| {
            cx.transform(to_target);
            cx.clip(pixels);
            cx.fill(pixels, &background_color(&self.background));

            cx.transform(to_pixels);
            for (node, state) in self.evaluate(t) {
                match DrawNode::new(node, &state) {
                    DrawNode::Path {
                        subpaths,
                        background,
                        fill,
                        stroke,
                        join,
                        cap,
                        ..
                    } => draw_path(cx, &subpaths, background, fill, stroke, join, cap)?,
                    DrawNode::Image {
                        image,
                        corners,
                        opacity,
                        smooth,
                        ..
                    } => draw_image(cx, &images.0[image], corners, opacity, smooth)?,
                }
            }
            Ok(())
        })
        .map_err(|e| anyhow!("drawing slide failed: {e:?}"))
    }
}

/// Pixel frame into `target`: uniform scale, centered (letterboxed).
fn fit(frame: &Frame, target: Rect) -> Affine {
    let scale = (target.width() / frame.pw).min(target.height() / frame.ph);
    let dx = target.x0 + (target.width() - frame.pw * scale) / 2.0;
    let dy = target.y0 + (target.height() - frame.ph * scale) / 2.0;
    Affine::translate((dx, dy)) * Affine::scale(scale)
}

/// Manim units (y up) to pixels (y down), section 4.1.
fn frame_matrix(f: &Frame) -> Affine {
    let sx = f.pw / f.fw;
    let sy = f.ph / f.fh;
    Affine::new([
        sx,
        0.0,
        0.0,
        -sy,
        f.pw / 2.0 - f.cx * sx,
        f.ph / 2.0 + f.cy * sy,
    ])
}

/// `("#RRGGBB", opacity)`; black for a malformed color.
fn background_color((hex, opacity): &(String, f64)) -> piet::Color {
    let channel = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|c| u8::from_str_radix(c, 16).ok())
            .unwrap_or(0)
    };
    let (r, g, b) = (channel(1), channel(3), channel(5));
    piet::Color::rgba8(r, g, b, (opacity.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn bez_path(subpaths: &[DrawSubpath]) -> BezPath {
    let mut path = BezPath::new();
    for sub in subpaths {
        path.move_to((sub.start[0], sub.start[1]));
        for c in &sub.cubics {
            path.curve_to((c[0], c[1]), (c[2], c[3]), (c[4], c[5]));
        }
        if sub.closed {
            path.close_path();
        }
    }
    path
}

fn brush<R: RenderContext>(cx: &mut R, paint: &Paint) -> Result<R::Brush, piet::Error> {
    match paint {
        Paint::Color([r, g, b, a]) => Ok(cx.solid_brush(piet::Color::rgba(*r, *g, *b, *a))),
        Paint::Gradient { line, stops } => {
            let last = (stops.len() - 1).max(1) as f32;
            let stops = stops
                .iter()
                .enumerate()
                .map(|(i, [r, g, b, a])| piet::GradientStop {
                    pos: i as f32 / last,
                    color: piet::Color::rgba(*r, *g, *b, *a),
                })
                .collect();
            let gradient = piet::FixedLinearGradient {
                start: (line[0], line[1]).into(),
                end: (line[2], line[3]).into(),
                stops,
            };
            cx.gradient(gradient)
        }
    }
}

fn stroke_style(join: Join, cap: Cap) -> StrokeStyle {
    let join = match join {
        Join::Miter => piet::LineJoin::Miter { limit: MITER_LIMIT },
        Join::Round => piet::LineJoin::Round,
        Join::Bevel => piet::LineJoin::Bevel,
    };
    let cap = match cap {
        Cap::Butt => piet::LineCap::Butt,
        Cap::Round => piet::LineCap::Round,
        Cap::Square => piet::LineCap::Square,
    };
    StrokeStyle::new().line_join(join).line_cap(cap)
}

/// Background stroke, then fill (nonzero), then stroke (section 10.1).
fn draw_path(
    cx: &mut impl RenderContext,
    subpaths: &[DrawSubpath],
    background: Option<Line>,
    fill: Option<Paint>,
    stroke: Option<Line>,
    join: Join,
    cap: Cap,
) -> Result<(), piet::Error> {
    let path = bez_path(subpaths);
    let style = stroke_style(join, cap);

    if let Some(line) = background {
        let brush = brush(cx, &line.paint)?;
        cx.stroke_styled(&path, &brush, line.width, &style);
    }

    if let Some(paint) = fill {
        let brush = brush(cx, &paint)?;
        cx.fill(&path, &brush);
    }

    if let Some(line) = stroke {
        let brush = brush(cx, &line.paint)?;
        cx.stroke_styled(&path, &brush, line.width, &style);
    }
    Ok(())
}

/// The bitmap over the parallelogram of its corners (section 5.3).
///
/// piet has no image opacity, so a fading image gets its alpha scaled here.
fn draw_image(
    cx: &mut impl RenderContext,
    bitmap: &Bitmap,
    corners: [f64; 6],
    opacity: f64,
    smooth: bool,
) -> Result<(), piet::Error> {
    let rgba: Vec<u8> = if opacity >= 1.0 {
        bitmap.rgba.clone()
    } else {
        bitmap
            .rgba
            .as_chunks::<RGBA_BYTES>()
            .0
            .iter()
            .flat_map(|&[r, g, b, a]| [r, g, b, (a as f64 * opacity).round() as u8])
            .collect()
    };
    let image = cx.make_image(
        bitmap.width,
        bitmap.height,
        &rgba,
        piet::ImageFormat::RgbaSeparate,
    )?;

    let [x0, y0, x1, y1, x2, y2] = corners;
    let unit_square = Affine::new([x1 - x0, y1 - y0, x2 - x0, y2 - y0, x0, y0]);
    let interpolation = if smooth {
        piet::InterpolationMode::Bilinear
    } else {
        piet::InterpolationMode::NearestNeighbor
    };

    cx.with_save(|cx| {
        cx.transform(unit_square);
        cx.draw_image(&image, Rect::new(0.0, 0.0, 1.0, 1.0), interpolation);
        Ok(())
    })
}
