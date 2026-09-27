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
/// Only the window geometry is stored. The camera is built per draw by
/// [`Presentation::camera_for`], so drawing needs no mutable engine access.
#[derive(Debug)]
pub struct Presentation {
    visible: bool,
    /// Size and scale factor only. Zoom and offset are per page.
    window: Camera,
    /// The page the audience is held on, `None` while the view follows the lecturer.
    locked_page: Option<Aabb>,
    blanked: bool,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            visible: false,
            window: Camera::default().with_size(Self::WINDOW_SIZE_DEFAULT),
            locked_page: None,
            blanked: false,
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

    pub fn blanked(&self) -> bool {
        self.blanked
    }

    /// Show the audience nothing but the letterbox.
    pub(crate) fn set_blanked(&mut self, blanked: bool) -> WidgetFlags {
        self.blanked = blanked;
        redraw()
    }

    pub fn page_locked(&self) -> bool {
        self.locked_page.is_some()
    }

    /// Hold the audience on `page`, or follow the lecturer again with `None`.
    pub(crate) fn lock_on(&mut self, page: Option<Aabb>) -> WidgetFlags {
        self.locked_page = page;
        redraw()
    }

    /// The page the audience sees: the locked one, else the lecturer's.
    pub(crate) fn page_or(&self, lecturer_page: Aabb) -> Aabb {
        self.locked_page.unwrap_or(lecturer_page)
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

        // The camera clamps the zoom; the offset must use the clamped value.
        let camera = self.window.clone().with_zoom(fitting_zoom);
        let zoom = camera.total_zoom();

        camera.with_offset(page.mins * zoom - (size - extents * zoom) * 0.5)
    }

    /// Fill the window with the letterbox color. The snapshot must be in surface coordinates.
    #[cfg(feature = "ui")]
    pub(crate) fn draw_bars_to_gtk_snapshot(
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

    /// Fill the page with the background color, without the pattern: ruling is a writing aid
    /// for the lecturer only.
    ///
    /// The snapshot must be transformed to document coordinates with the camera framing `page`.
    #[cfg(feature = "ui")]
    pub(crate) fn draw_paper_to_gtk_snapshot(
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

        let page = Aabb::new_positive(Vector2::new(0.0, 2000.0), Vector2::new(800.0, 3000.0));
        let viewport = presentation.camera_for(page).viewport();

        assert!(viewport.contains(&page));
        let margin_top = page.mins[1] - viewport.mins[1];
        let margin_bottom = viewport.maxs[1] - page.maxs[1];
        assert!((margin_top - margin_bottom).abs() < 1e-6);
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
}
