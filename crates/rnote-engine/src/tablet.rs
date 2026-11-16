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
    press: Press,
    /// Last pen position, in document coordinates.
    hover: Option<Vector2>,
}

/// What the current pen press is doing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Press {
    /// The pen is up or hovering.
    #[default]
    None,
    /// A stroke is running. It may leave the page, but only start on it.
    Drawing,
    /// The press began in the letterbox. It stays dropped until the pen lifts, also when
    /// dragged onto the page.
    Rejected,
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

                if self.press == Press::None {
                    self.press = if bounds.contains_local_point(element.pos) {
                        Press::Drawing
                    } else {
                        Press::Rejected
                    };
                }
                if self.press == Press::Rejected {
                    return None;
                }
            }
            PenEvent::Up { element, .. } | PenEvent::Proximity { element, .. } => {
                self.hover = Some(element.pos);

                // The pens never saw a rejected press, so they must not see its end either.
                let rejected = self.press == Press::Rejected;
                self.press = Press::None;
                if rejected {
                    return None;
                }
            }
            PenEvent::Cancel => {
                self.hover = None;
                self.press = Press::None;
            }
            PenEvent::KeyPressed { .. } | PenEvent::Text { .. } => {}
        }
        Some(event)
    }

    /// Forget the pen, e.g. when it leaves the tablet.
    pub(crate) fn clear(&mut self) -> WidgetFlags {
        self.press = Press::None;
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
        let _ = tablet.clear();
        assert!(tablet.filter_event(down(page.center()), page).is_some());
        assert!(tablet.filter_event(down(letterbox), page).is_some());
    }

    #[test]
    fn letterbox_press_stays_dropped() {
        let mut tablet = Tablet::default();
        let page = Aabb::new(Vector2::ZERO, Vector2::new(100.0, 100.0));
        let element = |pos| Element::new(pos, 1.0);
        let down = |pos| PenEvent::Down {
            element: element(pos),
            modifier_keys: Default::default(),
        };
        let up = |pos| PenEvent::Up {
            element: element(pos),
            modifier_keys: Default::default(),
        };
        let letterbox = Vector2::new(200.0, 50.0);

        // A press that began in the letterbox draws nothing, even once dragged onto the page.
        assert!(tablet.filter_event(down(letterbox), page).is_none());
        assert!(tablet.filter_event(down(page.center()), page).is_none());
        assert!(tablet.filter_event(up(page.center()), page).is_none());

        // The next press starts fresh.
        assert!(tablet.filter_event(down(page.center()), page).is_some());
    }
}
