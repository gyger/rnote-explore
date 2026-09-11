// Imports
#[cfg(feature = "ui")]
use crate::document::Background;
use crate::{Camera, WidgetFlags};
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;

/// The audience view: one whole page, seen through a camera of its own.
///
/// ```text
///  document (doc coords)             audience window (surface px)
///  ┌───────────────────────┐         ┌──────────────────────────┐
///  │ page  ┌──────────┐    │   fit   │ ┌──────────────────────┐ │
///  │       │ lecturer │    │  ─────► │ │      whole page      │ │
///  │       │   zoom   │    │         │ └──────────────────────┘ │
///  └───────────────────────┘         └──────────────────────────┘
/// ```
///
/// The lecturer zooms in to write; this camera stays on the whole page, so the audience
/// never follows that zoom.
///
/// Only the window geometry is kept here. The framed camera is built per draw by
/// [`Presentation::camera_for`], so following the lecturer's page needs no state of its
/// own and no mutable engine access while drawing.
#[derive(Debug)]
pub struct Presentation {
    visible: bool,
    /// Window geometry only: size and scale factor. Zoom and offset are per page.
    window: Camera,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            visible: false,
            window: Camera::default().with_size(Self::WINDOW_SIZE_DEFAULT),
        }
    }
}

impl Presentation {
    const WINDOW_SIZE_DEFAULT: Vector2 = Vector2::new(1280.0, 720.0);
    /// The bars beside a page that does not match the screen ratio.
    #[cfg(feature = "ui")]
    const LETTERBOX_COLOR: rnote_compose::Color = rnote_compose::Color::BLACK;

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub(crate) fn set_visible(&mut self, visible: bool) -> WidgetFlags {
        self.visible = visible;
        redraw()
    }

    /// Set the window size in surface pixels.
    pub(crate) fn set_size(&mut self, size: Vector2, scale_factor: f64) -> WidgetFlags {
        let _ = self.window.set_size(size);
        let _ = self.window.set_scale_factor(scale_factor);
        redraw()
    }

    /// The camera framing `page`: the whole page, centered in the window.
    pub fn camera_for(&self, page: Aabb) -> Camera {
        let size = self.window.size();
        let extents = page.extents();
        if extents[0] <= 0.0 || extents[1] <= 0.0 {
            return self.window.clone();
        }

        // The tighter axis decides, so a page of another ratio is letterboxed on the other one.
        let fitting_zoom = (size[0] / extents[0]).min(size[1] / extents[1]);

        // Build first: the camera clamps the zoom, and the offset must use the clamped value.
        let camera = self.window.clone().with_zoom(fitting_zoom);
        let zoom = camera.total_zoom();

        // Center: the surface left over beside the scaled page is split evenly.
        camera.with_offset(page.mins * zoom - (size - extents * zoom) * 0.5)
    }

    /// Fill the whole window with the letterbox color, behind everything else.
    ///
    /// The snapshot must be in surface coordinates.
    #[cfg(feature = "ui")]
    pub(crate) fn draw_letterbox_to_gtk_snapshot(
        &self,
        snapshot: &gtk4::Snapshot,
        surface_bounds: Aabb,
    ) {
        use crate::ext::{GdkRGBAExt, GrapheneRectExt};
        use gtk4::{gdk, graphene, gsk, prelude::*};

        snapshot.append_node(
            gsk::ColorNode::new(
                &gdk::RGBA::from_compose_color(Self::LETTERBOX_COLOR),
                &graphene::Rect::from_p2d_aabb(surface_bounds),
            )
            .upcast(),
        );
    }

    /// Fill the page with the plain background color.
    ///
    /// The pattern is a writing aid for the lecturer - ruling, grid, dots. The audience only
    /// needs the strokes, so the page goes out as clean paper.
    ///
    /// The snapshot must be transformed to document coordinates with the camera framing `page`.
    #[cfg(feature = "ui")]
    pub(crate) fn draw_background_to_gtk_snapshot(
        &self,
        snapshot: &gtk4::Snapshot,
        page: Aabb,
        background: &Background,
    ) {
        use crate::ext::{GdkRGBAExt, GrapheneRectExt};
        use gtk4::{gdk, graphene, gsk, prelude::*};

        snapshot.append_node(
            gsk::ColorNode::new(
                &gdk::RGBA::from_compose_color(background.color),
                &graphene::Rect::from_p2d_aabb(page),
            )
            .upcast(),
        );
    }
}

fn redraw() -> WidgetFlags {
    let mut widget_flags = WidgetFlags::default();
    widget_flags.redraw = true;
    widget_flags
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2d::bounding_volume::BoundingVolume;
    use rnote_compose::ext::AabbExt;

    fn presentation(window_size: Vector2) -> Presentation {
        let mut presentation = Presentation::default();
        let _ = presentation.set_size(window_size, 1.0);
        presentation
    }

    #[test]
    fn page_fits_centered() {
        let presentation = presentation(Vector2::new(1000.0, 1000.0));

        // A page further down the document, as the lecturer scrolls to it.
        let page = Aabb::new_positive(Vector2::new(0.0, 2000.0), Vector2::new(800.0, 3000.0));
        let viewport = presentation.camera_for(page).viewport();

        assert!(viewport.contains(&page));
        let margin_top = page.mins[1] - viewport.mins[1];
        let margin_bottom = viewport.maxs[1] - page.maxs[1];
        assert!((margin_top - margin_bottom).abs() < 1e-6);
    }

    #[test]
    fn matching_ratio_leaves_no_letterbox() {
        // A screen page on a screen of the same ratio must fill it edge to edge.
        let page_size = Vector2::new(1600.0, 900.0);
        let presentation = presentation(Vector2::new(1920.0, 1080.0));

        let page = Aabb::new_positive(Vector2::ZERO, page_size);
        let viewport = presentation.camera_for(page).viewport();

        assert!((viewport.mins - page.mins).length() < 1e-6);
        assert!((viewport.maxs - page.maxs).length() < 1e-6);
    }

    #[test]
    fn portrait_page_is_letterboxed_sideways() {
        // A portrait page on a wide screen: full height, bars left and right.
        let page_size = Vector2::new(800.0, 1000.0);
        let presentation = presentation(Vector2::new(1920.0, 1080.0));

        let page = Aabb::new_positive(Vector2::ZERO, page_size);
        let viewport = presentation.camera_for(page).viewport();

        assert!((viewport.extents()[1] - page_size[1]).abs() < 1e-6);
        assert!(viewport.extents()[0] > page_size[0]);
    }

    #[test]
    fn page_change_keeps_the_zoom() {
        let page_size = Vector2::new(800.0, 1000.0);
        let presentation = presentation(Vector2::new(1920.0, 1080.0));

        let first = Aabb::new_positive(Vector2::ZERO, page_size);
        let second = Aabb::new_positive(Vector2::new(0.0, 1000.0), Vector2::new(800.0, 2000.0));

        // Only the offset follows the page, so a page change never rescales the view.
        assert_eq!(
            presentation.camera_for(first).total_zoom(),
            presentation.camera_for(second).total_zoom()
        );
    }
}
