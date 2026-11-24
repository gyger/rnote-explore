// Imports
#[cfg(feature = "ui")]
use crate::Camera;
use crate::WidgetFlags;
use crate::engine::Engine;
#[cfg(feature = "ui")]
use crate::engine::EngineView;
use crate::presentation::AudienceColumn;
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

    /// Show the audience the lecturer's page, or the first column of the lecturer's row.
    pub fn presentation_set_audience_column(&mut self, column: AudienceColumn) -> WidgetFlags {
        self.presentation.set_column(column)
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

    /// The page under the center of the lecturer's viewport, clamped to the document.
    ///
    /// Pages tile from the coordinate origin, like `draw_format_borders_to_gtk_snapshot`, not
    /// from the document origin, which drifts with infinite layout padding.
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

    /// Draw the audience page, letterboxed.
    ///
    /// Strokes are drawn immediately: the store caches textures for the main camera zoom only.
    #[cfg(feature = "ui")]
    pub fn draw_audience_to_gtk_snapshot(
        &self,
        snapshot: &gtk4::Snapshot,
        surface_bounds: Aabb,
    ) -> anyhow::Result<()> {
        use crate::ext::GrapheneRectExt;
        use gtk4::{graphene, prelude::*};

        snapshot.push_clip(&graphene::Rect::from_p2d_aabb(surface_bounds));

        self.presentation
            .draw_bars_to_gtk_snapshot(snapshot, surface_bounds);
        if self.presentation.blanked() {
            snapshot.pop();
            return Ok(());
        }

        // Rasters are made in the background: the audience stays on its page until the
        // wanted one has its rasters.
        let wanted = self.audience_page();
        let rasters = self.request_audience_rasters(wanted);
        let page = self.presentation.settle_on(wanted, rasters);
        let camera = self.presentation.camera_for(page);

        // Like the margin around the main canvas' viewport: ready before it scrolls in.
        for beside in self.pages_beside(wanted) {
            let _ = self.request_audience_rasters(beside);
        }

        // Paper is drawn with the strokes: markers multiply with it.
        let paper = |snapshot: &gtk4::Snapshot| {
            snapshot.save();
            snapshot.transform(Some(&camera.transform_for_gtk_snapshot()));
            self.presentation.draw_paper_to_gtk_snapshot(
                snapshot,
                page,
                &self.document.config.background,
            );
            snapshot.restore();
        };

        self.store.draw_strokes_cached(
            snapshot,
            &camera,
            page,
            page,
            &self.audience_rasters,
            self.penholder.current_pen_progress(),
            &self.slides.view(crate::manimslide::SlideView::Audience),
            paper,
        )?;

        self.draw_laser_to_gtk_snapshot(snapshot, &camera)?;
        self.draw_tablet_to_gtk_snapshot(snapshot, &camera);

        snapshot.pop();
        Ok(())
    }

    /// Order the rasters the audience needs to see `page`.
    #[cfg(feature = "ui")]
    fn request_audience_rasters(&self, page: Aabb) -> crate::store::raster_cache::Rasters {
        self.store.request_rasters(
            &self.presentation.camera_for(page),
            page,
            &self.audience_rasters,
            self.penholder.current_pen_progress(),
            &self.slides.view(crate::manimslide::SlideView::Audience),
            &self.tasks_tx,
        )
    }

    /// The pages above and below `page` in the document: where scrolling leads next.
    ///
    /// Placed on the page grid like `lecturer_page`, not shifted from `page`: a raster made
    /// ahead fits only the exact page, and `row * height + height` can round differently
    /// than `(row + 1) * height`.
    #[cfg(feature = "ui")]
    fn pages_beside(&self, page: Aabb) -> impl Iterator<Item = Aabb> {
        use p2d::bounding_volume::BoundingVolume;

        const ABOVE: f64 = -1.0;
        const BELOW: f64 = 1.0;
        let doc_bounds = self.document.bounds();
        let page_size = self.document.config.format.size();
        // No grid without a page size: then there is no page beside.
        let on_grid = page_size[0] > 0.0 && page_size[1] > 0.0;
        let cell = (page.mins / page_size).round();

        [ABOVE, BELOW]
            .into_iter()
            .filter(move |_| on_grid)
            .map(move |side| {
                let mins = (cell + Vector2::new(0.0, side)) * page_size;
                Aabb::new(mins, mins + page_size)
            })
            .filter(move |beside| doc_bounds.intersects(beside))
    }

    /// Draw the tablet pen marker.
    #[cfg(feature = "ui")]
    fn draw_tablet_to_gtk_snapshot(&self, snapshot: &gtk4::Snapshot, camera: &Camera) {
        use gtk4::prelude::*;

        snapshot.save();
        snapshot.transform(Some(&camera.transform_for_gtk_snapshot()));
        self.tablet.draw_hover_to_gtk_snapshot(snapshot, camera);
        snapshot.restore();
    }

    /// Draw the laser pointer, the only pen overlay the audience sees.
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
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page_size = engine.document.config.format.size();
        engine.document.x = -137.0;
        engine.document.y = -211.0;
        engine.document.width = page_size[0] * 4.0;
        engine.document.height = page_size[1] * 4.0;

        let _ = engine.camera.set_viewport_center(page_size * 0.5);

        assert_eq!(engine.lecturer_page().mins, Vector2::ZERO);
    }

    /// A raster made ahead is only used if its page is exactly the one the audience gets.
    #[cfg(feature = "ui")]
    #[test]
    fn pages_beside_are_the_pages_scrolling_reaches() {
        const PAGES: usize = 40;
        // A4 at 96 dpi: `row * height + height` is not always `(row + 1) * height`.
        const PAGE_HEIGHT: f64 = 1122.5197;
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        engine.document.config.format.set_height(PAGE_HEIGHT);
        let page_size = engine.document.config.format.size();
        engine.document.height = page_size[1] * PAGES as f64;
        let mut page_at = |row: usize| {
            let center = Vector2::new(page_size[0] * 0.5, page_size[1] * (row as f64 + 0.5));
            let _ = engine.camera.set_viewport_center(center);
            engine.audience_page()
        };
        let pages: Vec<Aabb> = (0..PAGES).map(&mut page_at).collect();

        for row in 1..PAGES - 1 {
            let beside: Vec<Aabb> = engine.pages_beside(pages[row]).collect();
            assert_eq!(beside, vec![pages[row - 1], pages[row + 1]], "row {row}");
        }
    }

    #[test]
    fn first_column_shows_the_lecturers_row() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page_size = engine.document.config.format.size();
        engine.document.width = page_size[0] * 2.0;
        engine.document.height = page_size[1] * 2.0;
        let _ = engine.presentation_set_audience_column(AudienceColumn::First);

        // The lecturer looks at column 2 of row 2.
        let _ = engine
            .camera
            .set_viewport_center(Vector2::new(page_size[0] * 1.5, page_size[1] * 1.5));
        assert_eq!(engine.audience_page().mins, Vector2::new(0.0, page_size[1]));

        let _ = engine.presentation_set_audience_column(AudienceColumn::Follow);
        assert_eq!(engine.audience_page().mins, page_size);
    }

    #[test]
    fn locking_holds_the_page_the_audience_sees() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page_size = engine.document.config.format.size();
        engine.document.height = page_size[1] * 4.0;

        let locked = engine.audience_page();
        let _ = engine.presentation_set_page_locked(true);

        let _ = engine
            .camera
            .set_viewport_center(Vector2::new(page_size[0] * 0.5, page_size[1] * 1.5));
        assert_ne!(engine.lecturer_page(), locked);
        assert_eq!(engine.audience_page(), locked);

        let _ = engine.presentation_set_page_locked(false);
        assert_eq!(engine.audience_page(), engine.lecturer_page());
    }
}
