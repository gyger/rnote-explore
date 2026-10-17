//! Drawing an evaluated slide with piet.
//!
//! ```text
//!  target rect (document)  ◄── scale + center ──  pixel frame (pw × ph)  ◄── frame matrix ──  Manim units (y up)
//! ```

use super::draw::{DrawPath, DrawSubpath, Paint};
use super::timeline::{Cap, Frame, Join, Seen, Slide};
use anyhow::anyhow;
use kurbo::{Affine, BezPath, Rect};
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use piet::{RenderContext, StrokeStyle};

/// Cairo's default miter limit, which Manim draws with.
const MITER_LIMIT: f64 = 10.0;

/// Whether a drawing shows the cues: the lecturer's views do, the presentation window not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cues {
    Shown,
    Hidden,
}

impl Slide {
    /// The slide at `t` as a standalone SVG document of its pixel size (the lecturer's still
    /// frame).
    pub(crate) fn still_svg(&self, t: f64, cues: Cues) -> anyhow::Result<String> {
        let size = Vector2::new(self.frame.pw, self.frame.ph);
        let bounds = Aabb::new(Vector2::ZERO, size);
        let target = Rect::new(0.0, 0.0, size[0], size[1]);

        let svg = crate::Svg::gen_with_piet_cairo_backend(
            |cx| self.draw_at(cx, t, target, cues),
            bounds,
        )?;
        Ok(rnote_compose::utils::wrap_svg_root(
            &svg.svg_data,
            Some(bounds),
            Some(bounds),
            false,
        ))
    }

    /// Draw the slide at time `t` into `target`, centered with its aspect ratio kept.
    pub(crate) fn draw_at(
        &self,
        cx: &mut impl RenderContext,
        t: f64,
        target: Rect,
        cues: Cues,
    ) -> anyhow::Result<()> {
        let to_target = fit(&self.frame, target);
        let to_pixels = frame_matrix(&self.frame);
        let pixels = Rect::new(0.0, 0.0, self.frame.pw, self.frame.ph);

        cx.with_save(|cx| {
            cx.transform(to_target);
            cx.clip(pixels);
            cx.fill(pixels, &background_color(&self.background));

            cx.transform(to_pixels);
            for (_, state) in self.evaluate(t) {
                if cues == Cues::Hidden && state.seen == Seen::ByLecturer {
                    continue;
                }
                draw_path(cx, DrawPath::new(&state))?;
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

/// Background stroke, then fill (nonzero), then stroke.
fn draw_path(cx: &mut impl RenderContext, node: DrawPath) -> Result<(), piet::Error> {
    let path = bez_path(&node.subpaths);
    let style = stroke_style(node.join, node.cap);

    if let Some(line) = node.background {
        let brush = brush(cx, &line.paint)?;
        cx.stroke_styled(&path, &brush, line.width, &style);
    }

    if let Some(paint) = node.fill {
        let brush = brush(cx, &paint)?;
        cx.fill(&path, &brush);
    }

    if let Some(line) = node.stroke {
        let brush = brush(cx, &line.paint)?;
        cx.stroke_styled(&path, &brush, line.width, &style);
    }
    Ok(())
}
