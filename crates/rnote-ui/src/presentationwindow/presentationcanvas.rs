// Imports
use crate::RnCanvas;
use gtk4::{Widget, gdk, glib, graphene, prelude::*, subclass::prelude::*};
use rnote_engine::ext::GrapheneRectExt;
use std::cell::RefCell;
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_compose::ext::AabbExt;
use tracing::error;

mod imp {
    use super::*;

    /// The audience view. It has no engine of its own and takes no input, it only draws the
    /// followed canvas through the audience camera.
    #[derive(Debug, Default)]
    pub(crate) struct RnPresentationCanvas {
        pub(crate) canvas: glib::WeakRef<RnCanvas>,
        /// The still shown while frozen: what the audience saw at the moment of freezing.
        pub(crate) frozen_still: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RnPresentationCanvas {
        const NAME: &'static str = "RnPresentationCanvas";
        type Type = super::RnPresentationCanvas;
        type ParentType = Widget;
    }

    impl ObjectImpl for RnPresentationCanvas {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.add_css_class("view");
            obj.set_hexpand(true);
            obj.set_vexpand(true);
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RnPresentationCanvas {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            let obj = self.obj();

            let Some(canvas) = obj.canvas() else {
                return;
            };
            let _ = canvas.engine_mut().presentation_set_size(
                Vector2::new(width as f64, height as f64),
                obj.scale_factor() as f64,
            );
            obj.queue_draw();
        }

        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            let obj = self.obj();

            // Frozen: the audience keeps seeing the still, whatever the lecturer draws next.
            if let Some(still) = self.frozen_still.borrow().as_ref() {
                still.snapshot(snapshot, obj.width() as f64, obj.height() as f64);
                return;
            }

            let Some(canvas) = obj.canvas() else {
                return;
            };
            if let Err(e) = canvas
                .engine_ref()
                .draw_presentation_to_gtk_snapshot(snapshot, obj.bounds())
            {
                error!("Snapshot presentation canvas failed, Err: {e:?}");
            }
        }
    }
}

glib::wrapper! {
    pub(crate) struct RnPresentationCanvas(ObjectSubclass<imp::RnPresentationCanvas>)
        @extends Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for RnPresentationCanvas {
    fn default() -> Self {
        Self::new()
    }
}

impl RnPresentationCanvas {
    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    /// The followed canvas, if it still exists.
    pub(crate) fn canvas(&self) -> Option<RnCanvas> {
        self.imp().canvas.upgrade()
    }

    /// Follow `canvas`, `None` while the window is hidden.
    ///
    /// Only the followed engine keeps audience state, so the other tabs stay idle.
    pub(crate) fn set_canvas(&self, canvas: Option<&RnCanvas>) {
        if let Some(previous) = self.canvas() {
            let _ = previous.engine_mut().presentation_set_visible(false);
        }
        self.imp().canvas.set(canvas);

        if let Some(canvas) = canvas {
            let mut engine = canvas.engine_mut();
            let _ = engine.presentation_set_size(self.bounds().extents(), self.scale_factor() as f64);
            let _ = engine.presentation_set_visible(true);
        }
        self.queue_draw();
    }

    /// Hold the audience on a still of what they see now, or let the view live again.
    ///
    /// Locking the page still shows the ink as it is written. Freezing does not: the audience
    /// keeps the picture from the moment it was frozen, so the lecturer can prepare the next
    /// step in plain sight.
    pub(crate) fn set_frozen(&self, frozen: bool) {
        let still = frozen.then(|| self.capture_still()).flatten();
        self.imp().frozen_still.replace(still);
        self.queue_draw();
    }

    /// Render what the audience sees right now into a texture.
    fn capture_still(&self) -> Option<gdk::Texture> {
        let canvas = self.canvas()?;
        let renderer = self.native()?.renderer()?;
        let bounds = self.bounds();
        if bounds.extents()[0] <= 0.0 || bounds.extents()[1] <= 0.0 {
            return None;
        }

        // Drawn through the engine rather than the widget, so this cannot recurse into the
        // frozen branch of `snapshot()`.
        let snapshot = gtk4::Snapshot::new();
        if let Err(e) = canvas
            .engine_ref()
            .draw_presentation_to_gtk_snapshot(&snapshot, bounds)
        {
            error!("Capturing the presentation still failed, Err: {e:?}");
            return None;
        }

        let node = snapshot.to_node()?;
        Some(renderer.render_texture(&node, Some(&graphene::Rect::from_p2d_aabb(bounds))))
    }

    fn bounds(&self) -> Aabb {
        Aabb::new_positive(
            Vector2::ZERO,
            Vector2::new(self.width() as f64, self.height() as f64),
        )
    }
}
