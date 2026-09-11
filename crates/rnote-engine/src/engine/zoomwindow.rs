// Imports
use crate::WidgetFlags;
use crate::engine::Engine;
use crate::pens::PenMode;
use crate::zoomwindow::{BoxScale, BoxShift};
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
        self.zoom_window.shift_box(shift, &self.document)
    }

    pub fn zoom_window_scale_box(&mut self, scale: BoxScale) -> WidgetFlags {
        self.zoom_window
            .scale_box(scale, &self.document, &self.document.config.background)
    }

    pub fn zoom_window_new_line(&mut self) -> WidgetFlags {
        self.zoom_window.new_line(&self.document)
    }

    /// Handle a pen event coming from the panel. The element must already be in document coordinates.
    pub fn handle_zoom_window_pen_event(
        &mut self,
        event: PenEvent,
        pen_mode: Option<PenMode>,
        now: Instant,
    ) -> (EventPropagation, WidgetFlags) {
        self.handle_pen_event(event, pen_mode, now)
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
