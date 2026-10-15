// Imports
use crate::WidgetFlags;
use crate::engine::Engine;
use crate::pens::PenMode;
use p2d::glamx::DAffine2;
use rnote_compose::eventresult::EventPropagation;
use rnote_compose::penevent::PenEvent;
use std::time::Instant;

/// Pen input on the audience window. It writes into the same store as the main canvas.
impl Engine {
    /// Forget the tablet pen, e.g. when it leaves the tablet.
    pub fn tablet_clear(&mut self) -> WidgetFlags {
        self.tablet.clear()
    }

    /// Transform from audience window coordinates to document coordinates.
    pub fn audience_input_transform(&self) -> DAffine2 {
        self.presentation
            .camera_for(self.audience_page())
            .transform()
            .inverse()
    }

    /// Handle an audience window pen event, already in document coordinates. Strokes starting
    /// in the letterbox are dropped.
    pub fn handle_audience_pen_event(
        &mut self,
        event: PenEvent,
        pen_mode: Option<PenMode>,
        now: Instant,
    ) -> (EventPropagation, WidgetFlags) {
        // The hover marker moves with every event.
        let mut widget_flags = WidgetFlags::default();
        widget_flags.redraw = true;

        // A blank screen hides the ink, so writing blind is refused.
        if self.presentation.blanked() {
            return (EventPropagation::Stop, widget_flags);
        }

        let Some(event) = self.tablet.filter_event(event, self.audience_page()) else {
            return (EventPropagation::Stop, widget_flags);
        };

        // Straight to the pens: box dragging is for the main canvas only.
        let (propagation, wf) = self.penholder.handle_pen_event(
            event,
            pen_mode,
            now,
            &mut crate::engine_view_mut!(self),
        );
        (propagation, widget_flags | wf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use p2d::math::Vector2;

    #[test]
    fn audience_centre_is_page_centre() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let _ = engine.presentation_set_size(Vector2::new(1920.0, 1080.0), 1.0);
        let page = engine.audience_page();

        // The page is letterboxed, centred.
        let centre = engine
            .audience_input_transform()
            .transform_point2(Vector2::new(960.0, 540.0));
        assert_relative_eq!(centre, page.center(), epsilon = 1e-6);
    }
}
