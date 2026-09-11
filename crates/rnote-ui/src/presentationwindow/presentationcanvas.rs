// Imports
use crate::RnCanvas;
use gtk4::{Widget, glib, prelude::*, subclass::prelude::*};
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

    fn bounds(&self) -> Aabb {
        Aabb::new_positive(
            Vector2::ZERO,
            Vector2::new(self.width() as f64, self.height() as f64),
        )
    }
}
