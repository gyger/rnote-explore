// Imports
use crate::WidgetFlags;
use crate::engine::Engine;
use crate::pens::{PenMode, PenStyle};
use crate::zoomwindow::{BoxScale, BoxShift, LineStart};
use p2d::bounding_volume::BoundingVolume;
use p2d::math::Vector2;
use rnote_compose::eventresult::EventPropagation;
use rnote_compose::penevent::PenEvent;
use std::time::Instant;

/// Zoom window operations. The panel writes into the same store as the main canvas.
impl Engine {
    pub fn zoom_window_set_visible(&mut self, visible: bool) -> WidgetFlags {
        self.zoom_window.set_visible(visible)
    }

    /// Set the panel size in surface pixels.
    pub fn zoom_window_set_size(&mut self, size: Vector2) -> WidgetFlags {
        self.zoom_window
            .set_scale_factor(self.camera.scale_factor());
        self.zoom_window
            .set_size(size, &self.document, &self.document.config.background)
    }

    pub fn zoom_window_shift_box(&mut self, shift: BoxShift) -> WidgetFlags {
        self.zoom_window.shift_box(shift, &self.document) | self.reveal_zoom_box()
    }

    pub fn zoom_window_scale_box(&mut self, scale: BoxScale) -> WidgetFlags {
        self.zoom_window
            .scale_box(scale, &self.document, &self.document.config.background)
    }

    pub fn zoom_window_new_line(&mut self) -> WidgetFlags {
        self.zoom_window.new_line(&self.document) | self.reveal_zoom_box()
    }

    pub fn zoom_window_set_auto_advance(&mut self, auto_advance: bool) -> WidgetFlags {
        self.zoom_window.set_auto_advance(auto_advance)
    }

    pub fn zoom_window_set_return_height(&mut self, height: f64) {
        self.zoom_window.set_return_height(height);
    }

    pub fn zoom_window_set_line_start(&mut self, line_start: LineStart) {
        self.zoom_window.set_line_start(line_start);
    }

    /// Handle a pen event coming from the panel. The element must already be in document coordinates.
    ///
    /// A finished brush stroke may move the box forward (auto-advance).
    pub fn handle_zoom_window_pen_event(
        &mut self,
        event: PenEvent,
        pen_mode: Option<PenMode>,
        now: Instant,
    ) -> (EventPropagation, WidgetFlags) {
        let stroke_end = match (&event, self.current_pen_style_w_override()) {
            (PenEvent::Down { .. }, PenStyle::Brush) => {
                self.zoom_window.begin_stroke();
                None
            }
            (PenEvent::Up { element, .. }, PenStyle::Brush) => Some(element.pos),
            _ => None,
        };

        // Straight to the pens: the box drag interception is for the main canvas only.
        let (propagation, mut widget_flags) = self.penholder.handle_pen_event(
            event,
            pen_mode,
            now,
            &mut crate::engine_view_mut!(self),
        );
        if let Some(pos) = stroke_end {
            widget_flags |= self.zoom_window.advance_after_stroke(pos, &self.document);
            widget_flags |= self.reveal_zoom_box();
        }

        (propagation, widget_flags)
    }

    /// Scroll the main view so the whole box is visible, if it is not already.
    ///
    /// Writing in the panel moves the box across the page; the page should follow.
    pub fn reveal_zoom_box(&mut self) -> WidgetFlags {
        const MARGIN: f64 = 24.0;
        let viewport = self.camera.viewport();
        let bounds = self.zoom_window.box_bounds().loosened(MARGIN);
        if viewport.contains(&bounds) {
            return WidgetFlags::default();
        }

        // Smallest shift that brings the box inside, per axis.
        let shift = Vector2::new(
            axis_shift(
                viewport.mins[0],
                viewport.maxs[0],
                bounds.mins[0],
                bounds.maxs[0],
            ),
            axis_shift(
                viewport.mins[1],
                viewport.maxs[1],
                bounds.mins[1],
                bounds.maxs[1],
            ),
        );
        let offset = self.camera.offset() + shift * self.camera.total_zoom();
        self.camera.set_offset(offset, &self.document)
    }

    /// Draw the panel content: the document inside the box, magnified.
    ///
    /// Strokes are drawn immediately because the store caches textures for the main camera zoom only.
    #[cfg(feature = "ui")]
    pub fn draw_zoom_window_to_gtk_snapshot(
        &self,
        snapshot: &gtk4::Snapshot,
        surface_bounds: p2d::bounding_volume::Aabb,
    ) -> anyhow::Result<()> {
        use crate::drawable::DrawableOnDoc;
        use crate::engine::EngineView;
        use crate::ext::GrapheneRectExt;
        use gtk4::{graphene, prelude::*};
        use piet::RenderContext;
        use rnote_compose::ext::{AabbExt, DAffine2Ext};

        let camera = self.zoom_window.camera();
        let doc_bounds = self.document.bounds();
        let viewport = camera.viewport();

        snapshot.push_clip(&graphene::Rect::from_p2d_aabb(surface_bounds));

        snapshot.save();
        snapshot.transform(Some(&camera.transform_for_gtk_snapshot()));
        self.zoom_window.draw_background_to_gtk_snapshot(
            snapshot,
            doc_bounds,
            &self.document.config.background,
        );
        self.zoom_window.draw_advance_zone_to_gtk_snapshot(snapshot);
        snapshot.restore();

        // Transform on the piet side, so cairo rasterizes at panel resolution instead of upscaling.
        let cairo_cx = snapshot.append_cairo(&graphene::Rect::from_p2d_aabb(surface_bounds));
        let mut piet_cx = piet_cairo::CairoRenderContext::new(&cairo_cx);
        piet_cx.transform(camera.transform().to_kurbo());
        piet_cx.clip(doc_bounds.to_kurbo_rect());
        self.store
            .draw_strokes_immediate(&mut piet_cx, doc_bounds, viewport, camera.image_scale());
        piet_cx.finish().map_err(|e| anyhow::anyhow!("{e:?}"))?;

        let engine_view = EngineView {
            tasks_tx: self.tasks_tx.clone(),
            config: &self.config.read(),
            document: &self.document,
            store: &self.store,
            camera,
            audioplayer: &self.audioplayer,
            animation: &self.animation,
        };
        self.penholder
            .draw_on_doc_to_gtk_snapshot(snapshot, &engine_view)?;

        snapshot.pop();
        Ok(())
    }
}

/// Shift of a view interval so that a target interval fits, 0.0 if it already does.
fn axis_shift(view_min: f64, view_max: f64, target_min: f64, target_max: f64) -> f64 {
    if target_min < view_min {
        return target_min - view_min;
    }
    if target_max > view_max {
        return target_max - view_max;
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2d::bounding_volume::BoundingVolume;

    #[test]
    fn reveal_scrolls_to_box() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let _ = engine.zoom_window.set_visible(true);
        let far = Vector2::new(400.0, 3000.0);
        let _ = engine.zoom_window.move_box_to(far, &engine.document);
        assert!(
            !engine
                .camera
                .viewport()
                .contains(&engine.zoom_window.box_bounds())
        );

        let widget_flags = engine.reveal_zoom_box();

        assert!(widget_flags.view_modified);
        assert!(
            engine
                .camera
                .viewport()
                .contains(&engine.zoom_window.box_bounds())
        );
    }
}
