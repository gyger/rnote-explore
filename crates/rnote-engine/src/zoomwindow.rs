// Imports
use crate::document::Background;
use crate::{Camera, Document, Image, WidgetFlags};
use p2d::bounding_volume::{Aabb, BoundingVolume};
use p2d::math::Vector2;
use rnote_compose::penevent::PenEvent;
use tracing::error;

/// Direction to shift the zoom box along the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxShift {
    Left,
    Right,
}

/// Change of the zoom box size. A smaller box means a higher magnification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxScale {
    Shrink,
    Grow,
}

/// Where a new line starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineStart {
    /// Left edge of the page the box is on.
    #[default]
    PageEdge,
    /// The x where the box was last dragged to.
    LastPlaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Move,
    Resize,
}

/// A running drag of the box on the main canvas.
#[derive(Debug, Clone, Copy)]
struct BoxDrag {
    mode: DragMode,
    start_pos: Vector2,
    start_bounds: Aabb,
}

/// A magnified view of a small box on the document, for writing small and neat.
///
/// ```text
///  document (doc coords)                panel (surface px)
///  ┌───────────────────────┐            ┌──────────────────────┐
///  │   ┌─────────────┐     │   zoom     │                      │
///  │   │  box        │     │  ───────►  │   box content        │
///  │   └─────────────┘     │            │                      │
///  └───────────────────────┘            └──────────────────────┘
/// ```
///
/// The box is `camera.viewport()`, so the main canvas transform code applies
/// unchanged. Magnification is `panel width / box width`.
#[derive(Debug)]
pub struct ZoomWindow {
    visible: bool,
    camera: Camera,
    /// Requested box width in document units. The effective width also depends on the camera zoom limits.
    box_width: f64,
    /// Vertical distance of a new line, in document units.
    return_height: f64,
    line_start: LineStart,
    /// The x of the last explicit placement of the box.
    line_start_x: f64,
    /// Whether writing into the advance zone moves the box forward.
    auto_advance: bool,
    drag: Option<BoxDrag>,
    /// A stroke started in the panel and has not ended yet. Advance fires once per stroke.
    stroke_pending: bool,
    /// The main canvas pen is down. A stroke that started outside must not grab the box when it crosses it.
    canvas_pen_down: bool,
    // Background tile at the panel zoom, regenerated when the zoom or the background changes.
    tile_image: Option<Image>,
    #[cfg(feature = "ui")]
    tile_texture: Option<gtk4::gdk::MemoryTexture>,
}

impl Default for ZoomWindow {
    fn default() -> Self {
        let mut zoom_window = Self {
            visible: false,
            camera: Camera::default()
                .with_offset(Vector2::ZERO)
                .with_size(Self::PANEL_SIZE_DEFAULT),
            box_width: Self::BOX_WIDTH_DEFAULT,
            return_height: Self::RETURN_HEIGHT_DEFAULT,
            line_start: LineStart::default(),
            line_start_x: 0.0,
            auto_advance: true,
            drag: None,
            stroke_pending: false,
            canvas_pen_down: false,
            tile_image: None,
            #[cfg(feature = "ui")]
            tile_texture: None,
        };
        let _ = zoom_window.camera.zoom_to(zoom_window.fitting_zoom());
        zoom_window
    }
}

impl ZoomWindow {
    const PANEL_SIZE_DEFAULT: Vector2 = Vector2::new(800.0, 200.0);
    const BOX_WIDTH_DEFAULT: f64 = 300.0;
    const BOX_WIDTH_MIN: f64 = 50.0;
    const BOX_WIDTH_MAX: f64 = 2000.0;
    const BOX_SCALE_STEP: f64 = 1.25;
    /// Fraction of the box width moved by one shift.
    const BOX_SHIFT_FRACTION: f64 = 0.5;
    const RETURN_HEIGHT_DEFAULT: f64 = 32.0;
    /// Right part of the box that triggers the advance, as fraction of the box width.
    const ADVANCE_ZONE_FRACTION: f64 = 0.25;
    #[cfg(feature = "ui")]
    const BOX_BORDER_WIDTH: f64 = 1.5;
    /// Side of the resize handle square, in surface pixels.
    const HANDLE_SIZE: f64 = 14.0;
    #[cfg(feature = "ui")]
    const BOX_COLOR: piet::Color = rnote_compose::color::GNOME_BLUES[3];
    #[cfg(feature = "ui")]
    const ADVANCE_ZONE_ALPHA: f64 = 0.15;

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Show or hide the panel. A hidden panel holds no background tile and skips its regeneration.
    pub fn set_visible(&mut self, visible: bool, background: &Background) -> WidgetFlags {
        self.visible = visible;
        self.regenerate_background(background);
        redraw()
    }

    /// The camera of the panel. Its viewport is the box in document coordinates.
    pub fn camera(&self) -> &Camera {
        &self.camera
    }

    /// The box in document coordinates.
    pub fn box_bounds(&self) -> Aabb {
        self.camera.viewport()
    }

    pub fn return_height(&self) -> f64 {
        self.return_height
    }

    pub fn set_return_height(&mut self, height: f64) {
        self.return_height = height.max(1.0);
    }

    pub fn line_start(&self) -> LineStart {
        self.line_start
    }

    pub fn set_line_start(&mut self, line_start: LineStart) {
        self.line_start = line_start;
    }

    pub fn auto_advance(&self) -> bool {
        self.auto_advance
    }

    pub fn set_auto_advance(&mut self, auto_advance: bool) -> WidgetFlags {
        self.auto_advance = auto_advance;
        redraw()
    }

    /// The part of the box where finishing a stroke moves the box forward, in document coordinates.
    pub fn advance_zone(&self) -> Aabb {
        let bounds = self.box_bounds();
        let zone_start = bounds.maxs[0] - bounds.extents()[0] * Self::ADVANCE_ZONE_FRACTION;
        Aabb::new(Vector2::new(zone_start, bounds.mins[1]), bounds.maxs)
    }

    /// Note a stroke starting in the panel.
    pub(crate) fn begin_stroke(&mut self) {
        self.stroke_pending = true;
    }

    /// React to a finished stroke at `pos`.
    ///
    /// Ending a stroke in the advance zone shifts the box so the zone becomes its left part.
    /// Past the right document edge the box wraps to a new line instead.
    pub(crate) fn advance_after_stroke(&mut self, pos: Vector2, doc: &Document) -> WidgetFlags {
        // Repeated pen-up events (proximity, button quirks) must not advance again.
        let pending = std::mem::replace(&mut self.stroke_pending, false);
        if !pending || !self.auto_advance || !self.advance_zone().contains_local_point(pos) {
            return WidgetFlags::default();
        }

        let bounds = self.box_bounds();
        let shift = bounds.extents()[0] * (1.0 - Self::ADVANCE_ZONE_FRACTION);
        if bounds.maxs[0] + shift > doc.bounds().maxs[0] {
            return self.new_line(doc);
        }

        self.move_box_to(bounds.mins + Vector2::new(shift, 0.0), doc)
    }

    /// Handle a main canvas pen event that may move or resize the box.
    ///
    /// Pen down inside the box starts a move, on the bottom-right handle a resize.
    /// Returns `None` when the event is not for the box and the pens should see it.
    pub(crate) fn handle_box_event(
        &mut self,
        event: &PenEvent,
        camera: &Camera,
        doc: &Document,
        background: &Background,
    ) -> Option<WidgetFlags> {
        if !self.visible {
            return None;
        }

        match event {
            PenEvent::Down { element, .. } => {
                let first_down = !std::mem::replace(&mut self.canvas_pen_down, true);
                if self.drag.is_none() {
                    if !first_down {
                        return None;
                    }
                    self.drag = Some(self.drag_at(element.pos, camera)?);
                }
                self.drag_to(element.pos, doc, background);
                Some(redraw())
            }
            PenEvent::Up { element, .. } => {
                self.canvas_pen_down = false;
                self.drag?;
                self.drag_to(element.pos, doc, background);
                self.drag = None;
                Some(redraw())
            }
            PenEvent::Proximity { .. } | PenEvent::Cancel => {
                self.canvas_pen_down = false;
                self.drag.take().map(|_| redraw())
            }
            PenEvent::KeyPressed { .. } | PenEvent::Text { .. } => None,
        }
    }

    /// The drag a pen down at `pos` starts, if any.
    /// Touching just outside the border counts too, so a thin box is easy to grab.
    fn drag_at(&self, pos: Vector2, camera: &Camera) -> Option<BoxDrag> {
        let mode = if self.resize_handle_bounds(camera).contains_local_point(pos) {
            DragMode::Resize
        } else if self
            .box_bounds()
            .loosened(Self::HANDLE_SIZE / camera.total_zoom())
            .contains_local_point(pos)
        {
            DragMode::Move
        } else {
            return None;
        };

        Some(BoxDrag {
            mode,
            start_pos: pos,
            start_bounds: self.box_bounds(),
        })
    }

    fn drag_to(&mut self, pos: Vector2, doc: &Document, background: &Background) {
        let Some(drag) = self.drag else {
            return;
        };
        let delta = pos - drag.start_pos;

        match drag.mode {
            DragMode::Move => {
                let _ = self.place_box(drag.start_bounds.mins + delta, doc);
            }
            DragMode::Resize => {
                self.box_width = (drag.start_bounds.extents()[0] + delta[0])
                    .clamp(Self::BOX_WIDTH_MIN, Self::BOX_WIDTH_MAX);
                let _ = self.refit(drag.start_bounds.mins, doc);
                self.regenerate_background(background);
            }
        }
    }

    /// The resize handle: a square on the bottom-right corner, sized in surface pixels of `camera`.
    fn resize_handle_bounds(&self, camera: &Camera) -> Aabb {
        let half = Self::HANDLE_SIZE * 0.5 / camera.total_zoom();
        Aabb::from_half_extents(self.box_bounds().maxs, Vector2::splat(half))
    }

    /// Set the panel size in surface pixels. The box keeps its width and origin.
    pub(crate) fn set_size(
        &mut self,
        size: Vector2,
        doc: &Document,
        background: &Background,
    ) -> WidgetFlags {
        let origin = self.box_bounds().mins;
        let _ = self.camera.set_size(size);
        let widget_flags = self.refit(origin, doc);
        self.regenerate_background(background);
        widget_flags
    }

    pub(crate) fn set_scale_factor(&mut self, scale_factor: f64) {
        let _ = self.camera.set_scale_factor(scale_factor);
    }

    /// Move the box so that its top-left corner is at `origin` (document coordinates).
    pub(crate) fn move_box_to(&mut self, origin: Vector2, doc: &Document) -> WidgetFlags {
        let _ = self
            .camera
            .set_offset(origin * self.camera.total_zoom(), doc);
        redraw()
    }

    pub(crate) fn shift_box(&mut self, shift: BoxShift, doc: &Document) -> WidgetFlags {
        let step = self.box_bounds().extents()[0] * Self::BOX_SHIFT_FRACTION;
        let dx = match shift {
            BoxShift::Left => -step,
            BoxShift::Right => step,
        };
        let origin = self.box_bounds().mins + Vector2::new(dx, 0.0);
        self.move_box_to(origin, doc)
    }

    /// Place the box by hand. Its x becomes the start of following lines.
    pub(crate) fn place_box(&mut self, origin: Vector2, doc: &Document) -> WidgetFlags {
        let widget_flags = self.move_box_to(origin, doc);
        self.line_start_x = self.box_bounds().mins[0];
        widget_flags
    }

    /// Move the box to the start of the next line, one return height down.
    pub(crate) fn new_line(&mut self, doc: &Document) -> WidgetFlags {
        let x = match self.line_start {
            LineStart::PageEdge => self.page_left(doc),
            LineStart::LastPlaced => self.line_start_x,
        };
        let origin = Vector2::new(x, self.box_bounds().mins[1] + self.return_height);
        self.move_box_to(origin, doc)
    }

    /// Left edge of the page the box is on. Pages tile the plane aligned to the coordinate origin.
    fn page_left(&self, doc: &Document) -> f64 {
        let page_width = doc.config.format.size()[0];
        (self.box_bounds().mins[0] / page_width).floor() * page_width
    }

    pub(crate) fn scale_box(
        &mut self,
        scale: BoxScale,
        doc: &Document,
        background: &Background,
    ) -> WidgetFlags {
        let factor = match scale {
            BoxScale::Shrink => 1.0 / Self::BOX_SCALE_STEP,
            BoxScale::Grow => Self::BOX_SCALE_STEP,
        };
        self.box_width = (self.box_width * factor).clamp(Self::BOX_WIDTH_MIN, Self::BOX_WIDTH_MAX);

        let origin = self.box_bounds().mins;
        let widget_flags = self.refit(origin, doc);
        self.regenerate_background(background);
        widget_flags
    }

    /// Zoom that fits the requested box width into the panel width.
    fn fitting_zoom(&self) -> f64 {
        self.camera.size()[0] / self.box_width
    }

    /// Re-apply the zoom after the panel or the box width changed, keeping the box origin.
    fn refit(&mut self, origin: Vector2, doc: &Document) -> WidgetFlags {
        let _ = self.camera.zoom_to(self.fitting_zoom());
        self.move_box_to(origin, doc)
    }

    /// Regenerate the background tile for the panel zoom. Nothing is kept while the panel is hidden.
    pub(crate) fn regenerate_background(&mut self, background: &Background) {
        if !self.visible {
            self.tile_image = None;
            #[cfg(feature = "ui")]
            {
                self.tile_texture = None;
            }
            return;
        }

        match background.gen_tile_image(self.camera.image_scale()) {
            Ok(image) => self.tile_image = Some(image),
            Err(e) => {
                error!("Generating zoom window background tile failed, Err: {e:?}");
                return;
            }
        }

        #[cfg(feature = "ui")]
        {
            self.tile_texture = self.tile_image.as_ref().and_then(|image| {
                image
                    .to_memtexture()
                    .inspect_err(|e| {
                        error!("Generating zoom window background texture failed, Err: {e:?}")
                    })
                    .ok()
            });
        }
    }

    /// Draw the document background inside the box.
    ///
    /// The snapshot must be transformed to document coordinates with the panel camera.
    #[cfg(feature = "ui")]
    pub(crate) fn draw_background_to_gtk_snapshot(
        &self,
        snapshot: &gtk4::Snapshot,
        doc_bounds: Aabb,
        background: &Background,
    ) {
        use crate::ext::{GdkRGBAExt, GrapheneRectExt};
        use gtk4::{gdk, graphene, gsk, prelude::*};
        use rnote_compose::SplitOrder;
        use rnote_compose::ext::AabbExt;

        let viewport = self.box_bounds();

        snapshot.push_clip(&graphene::Rect::from_p2d_aabb(doc_bounds));
        snapshot.append_node(
            gsk::ColorNode::new(
                &gdk::RGBA::from_compose_color(background.color),
                &graphene::Rect::from_p2d_aabb(viewport),
            )
            .upcast(),
        );

        if let Some(texture) = &self.tile_texture {
            for tile_bounds in viewport
                .split_extended_origin_aligned(background.tile_size(), SplitOrder::default())
            {
                snapshot.append_node(
                    gsk::TextureNode::new(texture, &graphene::Rect::from_p2d_aabb(tile_bounds))
                        .upcast(),
                );
            }
        }

        snapshot.pop();
    }

    /// Draw the box outline on the main canvas.
    ///
    /// The snapshot must be transformed to document coordinates with the main `camera`.
    #[cfg(feature = "ui")]
    pub(crate) fn draw_box_to_gtk_snapshot(&self, snapshot: &gtk4::Snapshot, camera: &Camera) {
        use crate::ext::{GdkRGBAExt, GrapheneRectExt};
        use gtk4::{gdk, graphene, gsk, prelude::*};

        if !self.visible {
            return;
        }

        let border_width = (Self::BOX_BORDER_WIDTH / camera.total_zoom()) as f32;
        let color = gdk::RGBA::from_compose_color(Self::BOX_COLOR.into());
        let rounded_rect = gsk::RoundedRect::new(
            graphene::Rect::from_p2d_aabb(self.box_bounds()),
            graphene::Size::zero(),
            graphene::Size::zero(),
            graphene::Size::zero(),
            graphene::Size::zero(),
        );

        snapshot.append_border(&rounded_rect, &[border_width; 4], &[color; 4]);
        snapshot.append_node(
            gsk::ColorNode::new(
                &color,
                &graphene::Rect::from_p2d_aabb(self.resize_handle_bounds(camera)),
            )
            .upcast(),
        );
        self.draw_advance_zone_to_gtk_snapshot(snapshot);
    }

    /// Tint the advance zone. The snapshot must be transformed to document coordinates.
    #[cfg(feature = "ui")]
    pub(crate) fn draw_advance_zone_to_gtk_snapshot(&self, snapshot: &gtk4::Snapshot) {
        use crate::ext::{GdkRGBAExt, GrapheneRectExt};
        use gtk4::{gdk, graphene, gsk, prelude::*};

        if !self.auto_advance {
            return;
        }

        let mut color: rnote_compose::Color = Self::BOX_COLOR.into();
        color.a = Self::ADVANCE_ZONE_ALPHA;
        snapshot.append_node(
            gsk::ColorNode::new(
                &gdk::RGBA::from_compose_color(color),
                &graphene::Rect::from_p2d_aabb(self.advance_zone()),
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
    use approx::assert_relative_eq;
    use rnote_compose::penpath::Element;

    #[test]
    fn hidden_panel_has_no_tile() {
        let background = Background::default();
        let mut zoom_window = ZoomWindow::default();

        zoom_window.regenerate_background(&background);
        assert!(zoom_window.tile_image.is_none());

        let _ = zoom_window.set_visible(true, &background);
        assert!(zoom_window.tile_image.is_some());

        let _ = zoom_window.set_visible(false, &background);
        zoom_window.regenerate_background(&background);
        assert!(zoom_window.tile_image.is_none());
    }

    #[test]
    fn panel_resize_keeps_box() {
        let doc = Document::default();
        let mut zoom_window = ZoomWindow::default();
        let before = zoom_window.box_bounds();

        zoom_window.set_size(Vector2::new(1200.0, 300.0), &doc, &Background::default());
        let after = zoom_window.box_bounds();

        assert_relative_eq!(before.extents()[0], after.extents()[0]);
        assert_relative_eq!(before.mins, after.mins);
    }

    #[test]
    fn shrink_raises_magnification() {
        let doc = Document::default();
        let mut zoom_window = ZoomWindow::default();
        let zoom_before = zoom_window.camera().total_zoom();

        zoom_window.scale_box(BoxScale::Shrink, &doc, &Background::default());

        assert!(zoom_window.camera().total_zoom() > zoom_before);
    }

    #[test]
    fn stroke_in_zone_advances() {
        let doc = Document::default();
        let mut zoom_window = ZoomWindow::default();
        let before = zoom_window.box_bounds();

        zoom_window.begin_stroke();
        zoom_window.advance_after_stroke(before.center() - Vector2::new(1.0, 0.0), &doc);
        assert_relative_eq!(zoom_window.box_bounds().mins, before.mins);

        zoom_window.begin_stroke();
        zoom_window.advance_after_stroke(zoom_window.advance_zone().center(), &doc);
        let after = zoom_window.box_bounds();
        assert!(after.mins[0] > before.mins[0]);
        assert_relative_eq!(after.mins[1], before.mins[1]);
    }

    #[test]
    fn stroke_at_page_edge_wraps() {
        let doc = Document::default();
        let mut zoom_window = ZoomWindow::default();
        let width = zoom_window.box_bounds().extents()[0];
        zoom_window.move_box_to(Vector2::new(doc.bounds().maxs[0] - width, 100.0), &doc);

        zoom_window.begin_stroke();
        zoom_window.advance_after_stroke(zoom_window.advance_zone().center(), &doc);
        let bounds = zoom_window.box_bounds();

        assert_relative_eq!(bounds.mins[0], doc.bounds().mins[0]);
        assert_relative_eq!(bounds.mins[1], 100.0 + ZoomWindow::RETURN_HEIGHT_DEFAULT);
    }

    #[test]
    fn drag_inside_box_moves_it() {
        let doc = Document::default();
        let camera = Camera::default();
        let background = Background::default();
        let mut zoom_window = ZoomWindow::default();
        let _ = zoom_window.set_visible(true, &background);
        let before = zoom_window.box_bounds();
        let start = before.center();
        let delta = Vector2::new(40.0, 20.0);

        let outside = PenEvent::Down {
            element: Element::new(before.maxs + Vector2::splat(100.0), 1.0),
            modifier_keys: Default::default(),
        };
        assert!(
            zoom_window
                .handle_box_event(&outside, &camera, &doc, &background)
                .is_none()
        );
        let lift = PenEvent::Up {
            element: Element::new(before.maxs + Vector2::splat(100.0), 1.0),
            modifier_keys: Default::default(),
        };
        zoom_window.handle_box_event(&lift, &camera, &doc, &background);

        for event in [
            PenEvent::Down {
                element: Element::new(start, 1.0),
                modifier_keys: Default::default(),
            },
            PenEvent::Down {
                element: Element::new(start + delta, 1.0),
                modifier_keys: Default::default(),
            },
            PenEvent::Up {
                element: Element::new(start + delta, 1.0),
                modifier_keys: Default::default(),
            },
        ] {
            assert!(
                zoom_window
                    .handle_box_event(&event, &camera, &doc, &background)
                    .is_some()
            );
        }

        assert_relative_eq!(zoom_window.box_bounds().mins, before.mins + delta);
    }

    #[test]
    fn stroke_crossing_box_passes_through() {
        let doc = Document::default();
        let camera = Camera::default();
        let background = Background::default();
        let mut zoom_window = ZoomWindow::default();
        let _ = zoom_window.set_visible(true, &background);
        let before = zoom_window.box_bounds();

        let outside = PenEvent::Down {
            element: Element::new(before.maxs + Vector2::splat(100.0), 1.0),
            modifier_keys: Default::default(),
        };
        let inside = PenEvent::Down {
            element: Element::new(before.center(), 1.0),
            modifier_keys: Default::default(),
        };
        let up = PenEvent::Up {
            element: Element::new(before.center(), 1.0),
            modifier_keys: Default::default(),
        };
        for event in [&outside, &inside, &up] {
            assert!(
                zoom_window
                    .handle_box_event(event, &camera, &doc, &background)
                    .is_none()
            );
        }

        assert_relative_eq!(zoom_window.box_bounds().mins, before.mins);
    }

    #[test]
    fn drag_handle_resizes_box() {
        let doc = Document::default();
        let camera = Camera::default();
        let background = Background::default();
        let mut zoom_window = ZoomWindow::default();
        let _ = zoom_window.set_visible(true, &background);
        let before = zoom_window.box_bounds();

        let down = PenEvent::Down {
            element: Element::new(before.maxs, 1.0),
            modifier_keys: Default::default(),
        };
        let up = PenEvent::Up {
            element: Element::new(before.maxs + Vector2::new(50.0, 0.0), 1.0),
            modifier_keys: Default::default(),
        };
        zoom_window.handle_box_event(&down, &camera, &doc, &background);
        zoom_window.handle_box_event(&up, &camera, &doc, &background);
        let after = zoom_window.box_bounds();

        assert_relative_eq!(after.mins, before.mins);
        assert_relative_eq!(after.extents()[0], before.extents()[0] + 50.0);
    }

    #[test]
    fn new_line_returns_to_page_edge() {
        let mut doc = Document::default();
        doc.x = -1000.0;
        doc.width = 3000.0;
        let page_width = doc.config.format.size()[0];
        let mut zoom_window = ZoomWindow::default();
        zoom_window.place_box(Vector2::new(page_width + 200.0, 100.0), &doc);

        zoom_window.new_line(&doc);
        let bounds = zoom_window.box_bounds();

        assert_relative_eq!(bounds.mins[0], page_width);
        assert_relative_eq!(bounds.mins[1], 100.0 + ZoomWindow::RETURN_HEIGHT_DEFAULT);
    }

    #[test]
    fn new_line_returns_to_line_start() {
        let doc = Document::default();
        let mut zoom_window = ZoomWindow::default();
        zoom_window.set_line_start(LineStart::LastPlaced);
        zoom_window.place_box(Vector2::new(200.0, 100.0), &doc);
        zoom_window.shift_box(BoxShift::Right, &doc);

        zoom_window.new_line(&doc);
        let bounds = zoom_window.box_bounds();

        assert_relative_eq!(bounds.mins[0], 200.0);
        assert_relative_eq!(bounds.mins[1], 100.0 + ZoomWindow::RETURN_HEIGHT_DEFAULT);
    }
}
