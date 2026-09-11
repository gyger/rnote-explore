// Imports
use crate::WidgetFlags;
use crate::engine::Engine;
#[cfg(feature = "ui")]
use crate::engine::EngineView;
#[cfg(feature = "ui")]
use crate::Camera;
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;

/// Audience window operations. It reads the same store as the main canvas, through its own camera.
impl Engine {
    pub fn presentation_set_visible(&mut self, visible: bool) -> WidgetFlags {
        self.presentation.set_visible(visible)
    }

    /// Set the audience window size in surface pixels.
    pub fn presentation_set_size(&mut self, size: Vector2, scale_factor: f64) -> WidgetFlags {
        self.presentation.set_size(size, scale_factor)
    }

    pub fn presentation_set_blanked(&mut self, blanked: bool) -> WidgetFlags {
        self.presentation.set_blanked(blanked)
    }

    /// Hold the audience on the page they are seeing, or let them follow the lecturer again.
    pub fn presentation_set_page_locked(&mut self, locked: bool) -> WidgetFlags {
        let page = locked.then(|| self.audience_page());
        self.presentation.lock_on(page)
    }

    /// The page the audience is seeing.
    pub fn audience_page(&self) -> Aabb {
        self.presentation.page_or(self.lecturer_page())
    }

    /// The page under the center of the lecturer's viewport.
    ///
    /// Only the page follows the lecturer; zoom and scrolling inside a page do not reach the
    /// audience camera.
    ///
    /// Pages tile the plane from the coordinate origin, not from the document origin - the
    /// document origin drifts with the infinite layout padding while the page borders do not
    /// (see `draw_format_borders_to_gtk_snapshot`). The page is the grid cell the viewport
    /// center falls into, clamped to the cells the document actually spans.
    fn lecturer_page(&self) -> Aabb {
        let page_size = self.document.config.format.size();
        if page_size[0] <= 0.0 || page_size[1] <= 0.0 {
            return self.document.bounds();
        }

        let doc_bounds = self.document.bounds();
        let first_cell = (doc_bounds.mins / page_size).floor();
        let last_cell = (doc_bounds.maxs / page_size).ceil() - Vector2::ONE;
        let cell = (self.camera.viewport_center() / page_size)
            .floor()
            .clamp(first_cell, last_cell.max(first_cell));

        let mins = cell * page_size;
        Aabb::new(mins, mins + page_size)
    }

    /// Draw the framed page: background, then the strokes on it.
    ///
    /// Strokes are drawn immediately because the store caches textures for the main camera zoom only.
    #[cfg(feature = "ui")]
    pub fn draw_presentation_to_gtk_snapshot(
        &self,
        snapshot: &gtk4::Snapshot,
        surface_bounds: Aabb,
    ) -> anyhow::Result<()> {
        use crate::ext::GrapheneRectExt;
        use gtk4::{graphene, prelude::*};
        use piet::RenderContext;
        use rnote_compose::ext::{AabbExt, DAffine2Ext};

        let page = self.audience_page();
        let camera = self.presentation.camera_for(page);
        let doc_bounds = self.document.bounds();

        snapshot.push_clip(&graphene::Rect::from_p2d_aabb(surface_bounds));

        // Everything beside the page is letterbox, so a page that does not match the screen
        // ratio shows bars instead of the neighbouring pages.
        self.presentation
            .draw_letterbox_to_gtk_snapshot(snapshot, surface_bounds);

        // Blanked: the letterbox is the whole picture.
        if self.presentation.blanked() {
            snapshot.pop();
            return Ok(());
        }

        snapshot.save();
        snapshot.transform(Some(&camera.transform_for_gtk_snapshot()));
        self.presentation.draw_background_to_gtk_snapshot(
            snapshot,
            page,
            &self.document.config.background,
        );
        snapshot.restore();

        // Transform on the piet side, so cairo rasterizes at window resolution instead of upscaling.
        let cairo_cx = snapshot.append_cairo(&graphene::Rect::from_p2d_aabb(surface_bounds));
        let mut piet_cx = piet_cairo::CairoRenderContext::new(&cairo_cx);
        piet_cx.transform(camera.transform().to_kurbo());
        piet_cx.clip(page.to_kurbo_rect());
        self.store
            .draw_strokes_immediate(&mut piet_cx, doc_bounds, page, camera.image_scale());
        piet_cx.finish().map_err(|e| anyhow::anyhow!("{e:?}"))?;

        self.draw_laser_to_gtk_snapshot(snapshot, &camera)?;

        snapshot.pop();
        Ok(())
    }

    /// Draw the laser pointer trail on the audience view, through the audience camera.
    ///
    /// The audience gets no pen overlays - selection handles, pen indicators and the like are
    /// the lecturer's business. The laser is the exception: pointing at the page is meant for
    /// the audience, so it is the one overlay that must reach the projector.
    #[cfg(feature = "ui")]
    fn draw_laser_to_gtk_snapshot(
        &self,
        snapshot: &gtk4::Snapshot,
        camera: &Camera,
    ) -> anyhow::Result<()> {
        use crate::drawable::DrawableOnDoc;
        use crate::pens::PenStyle;
        use crate::pens::pensconfig::toolsconfig::ToolStyle;

        let config = self.config.read();
        let laser_active = self.current_pen_style_w_override() == PenStyle::Tools
            && config.pens_config.tools_config.style == ToolStyle::Laser;
        if !laser_active {
            return Ok(());
        }

        // The penholder draws through the camera it is handed, so the audience camera puts the
        // trail on the page where the lecturer is pointing.
        let engine_view = EngineView {
            tasks_tx: self.tasks_tx.clone(),
            config: &config,
            document: &self.document,
            store: &self.store,
            camera,
            audioplayer: &self.audioplayer,
            animation: &self.animation,
        };
        self.penholder
            .draw_on_doc_to_gtk_snapshot(snapshot, &engine_view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_grid_is_anchored_at_the_origin() {
        // The document origin drifts with the infinite layout padding, while the page borders
        // stay on the grid anchored at (0.0, 0.0). The frame must sit on the same grid.
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page_size = engine.document.config.format.size();
        engine.document.x = -137.0;
        engine.document.y = -211.0;
        engine.document.width = page_size[0] * 4.0;
        engine.document.height = page_size[1] * 4.0;

        let _ = engine
            .camera
            .set_viewport_center(page_size * 0.5);

        assert_eq!(engine.lecturer_page().mins, Vector2::ZERO);
    }

    #[test]
    fn locking_holds_the_page_the_audience_sees() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page_size = engine.document.config.format.size();
        engine.document.height = page_size[1] * 4.0;

        let locked = engine.audience_page();
        let _ = engine.presentation_set_page_locked(true);

        // The lecturer looks ahead at the next page.
        let _ = engine
            .camera
            .set_viewport_center(Vector2::new(page_size[0] * 0.5, page_size[1] * 1.5));
        assert_ne!(engine.lecturer_page(), locked);
        assert_eq!(engine.audience_page(), locked);

        // Unlocking catches the audience up.
        let _ = engine.presentation_set_page_locked(false);
        assert_eq!(engine.audience_page(), engine.lecturer_page());
    }

    #[test]
    fn lecturer_page_follows_the_viewport_center() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page_size = engine.document.config.format.size();

        let _ = engine
            .camera
            .set_viewport_center(Vector2::new(page_size[0] * 0.5, page_size[1] * 1.5));

        // The document is one page tall by default, so the second page is clamped back to it.
        assert_eq!(engine.lecturer_page().mins[1], 0.0);
    }
}
