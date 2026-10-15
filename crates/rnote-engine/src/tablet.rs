// Imports
use crate::WidgetFlags;
#[cfg(feature = "ui")]
use crate::Camera;
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_compose::penevent::PenEvent;

/// Pen input on the audience window, for a screenless tablet mapped to the projector.
///
/// ```text
///  projector (= tablet)               document
///  ┌──┬────────────────┬──┐          ┌────── page ──────┐
///  │  │                │  │  camera  │                  │
///  │  │   shown page   │  │  ──────► │   same spot      │
///  │  │                │  │  inverse │                  │
///  └──┴────────────────┴──┘          └──────────────────┘
///   letterbox = dead zone
/// ```
///
/// Strokes cannot start in the letterbox. The hover marker is the only cue a screenless
/// tablet gives.
#[derive(Debug, Default)]
pub struct Tablet {
    /// A stroke is running. It may leave the page, but only start on it.
    pen_down: bool,
    /// Last pen position, in document coordinates.
    hover: Option<Vector2>,
}

impl Tablet {
    /// Radius of the hover marker, in surface pixels.
    #[cfg(feature = "ui")]
    const HOVER_RADIUS: f64 = 6.0;
    #[cfg(feature = "ui")]
    const LINE_WIDTH: f64 = 1.5;
    #[cfg(feature = "ui")]
    const COLOR: piet::Color = rnote_compose::color::GNOME_BLUES[3];

    /// Track the hover position and drop strokes starting outside `bounds`.
    ///
    /// Returns `None` when the pens must not see the event.
    pub(crate) fn filter_event(&mut self, event: PenEvent, bounds: Aabb) -> Option<PenEvent> {
        match &event {
            PenEvent::Down { element, .. } => {
                self.hover = Some(element.pos);
                if !self.pen_down && !bounds.contains_local_point(element.pos) {
                    return None;
                }
                self.pen_down = true;
            }
            PenEvent::Up { element, .. } | PenEvent::Proximity { element, .. } => {
                self.hover = Some(element.pos);
                self.pen_down = false;
            }
            PenEvent::Cancel => {
                self.hover = None;
                self.pen_down = false;
            }
            PenEvent::KeyPressed { .. } | PenEvent::Text { .. } => {}
        }
        Some(event)
    }

    /// Forget the pen, e.g. when it leaves the tablet.
    pub(crate) fn clear(&mut self) -> WidgetFlags {
        self.pen_down = false;
        self.hover = None;
        let mut widget_flags = WidgetFlags::default();
        widget_flags.redraw = true;
        widget_flags
    }

    /// Ring at the pen position. The snapshot must be transformed to document coordinates with
    /// `camera`.
    #[cfg(feature = "ui")]
    pub(crate) fn draw_hover_to_gtk_snapshot(&self, snapshot: &gtk4::Snapshot, camera: &Camera) {
        use crate::ext::{GdkRGBAExt, GrapheneRectExt};
        use gtk4::{gdk, graphene, gsk, prelude::*};

        let Some(pos) = self.hover else {
            return;
        };

        let zoom = camera.total_zoom();
        let radius = Self::HOVER_RADIUS / zoom;
        let corner = graphene::Size::new(radius as f32, radius as f32);
        let ring = gsk::RoundedRect::new(
            graphene::Rect::from_p2d_aabb(Aabb::from_half_extents(pos, Vector2::splat(radius))),
            corner,
            corner,
            corner,
            corner,
        );
        let width = (Self::LINE_WIDTH / zoom) as f32;
        let color = gdk::RGBA::from_compose_color(Self::COLOR.into());

        snapshot.append_border(&ring, &[width; 4], &[color; 4]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use rnote_compose::penpath::Element;

    #[test]
    fn stroke_starts_on_page_only() {
        let mut tablet = Tablet::default();
        let page = Aabb::new(Vector2::ZERO, Vector2::new(100.0, 100.0));
        let down = |pos| PenEvent::Down {
            element: Element::new(pos, 1.0),
            modifier_keys: Default::default(),
        };
        let letterbox = Vector2::new(200.0, 50.0);

        // Pen down in the letterbox is dropped, but still moves the hover marker.
        assert!(tablet.filter_event(down(letterbox), page).is_none());
        assert_relative_eq!(tablet.hover.unwrap(), letterbox);

        // A stroke started on the page may leave it.
        assert!(tablet.filter_event(down(page.center()), page).is_some());
        assert!(tablet.filter_event(down(letterbox), page).is_some());
    }
}
