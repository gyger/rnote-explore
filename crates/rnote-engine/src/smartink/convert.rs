//! Conversions between engine strokes and wire strokes.

// Imports
use super::protocol::{InkPoint, InkStroke};
use crate::strokes::BrushStroke;
use p2d::math::Vector2;
use rnote_compose::penpath::Element;
use rnote_compose::{PenPath, Style};

/// A pen path needs a start and at least one segment.
const MIN_POINTS: usize = 2;

pub(crate) fn ink_from_brush(brush: &BrushStroke) -> InkStroke {
    let points = brush
        .path
        .clone()
        .into_elements()
        .into_iter()
        .map(|el| InkPoint {
            x: el.pos[0],
            y: el.pos[1],
            pressure: el.pressure,
        })
        .collect();
    let color = brush.style.stroke_color().unwrap_or_default();

    InkStroke {
        points,
        width: brush.style.stroke_width(),
        color: [color.r, color.g, color.b, color.a],
    }
}

/// Rebuild a brush stroke from wire points. The style comes from the caller;
/// width and color on the wire are ignored in protocol v0.
pub(crate) fn brush_from_ink(ink: &InkStroke, style: Style) -> Option<BrushStroke> {
    if ink.points.len() < MIN_POINTS {
        return None;
    }

    let path = PenPath::try_from_elements(
        ink.points
            .iter()
            .map(|p| Element::new(Vector2::new(p.x, p.y), p.pressure.clamp(0.0, 1.0))),
    )?;

    Some(BrushStroke::from_penpath(path, style))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brush() -> BrushStroke {
        let path = PenPath::try_from_elements([
            Element::new(Vector2::new(0.0, 0.0), 0.2),
            Element::new(Vector2::new(1.0, 2.0), 0.4),
            Element::new(Vector2::new(3.0, 1.0), 0.6),
        ])
        .unwrap();
        BrushStroke::from_penpath(path, Style::default())
    }

    #[test]
    fn round_trip_keeps_points() {
        let original = brush();
        let ink = ink_from_brush(&original);
        let back = brush_from_ink(&ink, original.style.clone()).unwrap();

        assert_eq!(
            back.path.clone().into_elements(),
            original.path.clone().into_elements()
        );
    }

    #[test]
    fn single_point_is_rejected() {
        let ink = InkStroke {
            points: vec![[0.0, 0.0, 0.5].into()],
            width: 1.0,
            color: [0.0; 4],
        };
        assert!(brush_from_ink(&ink, Style::default()).is_none());
    }
}
