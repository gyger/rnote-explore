// Imports
use crate::RnCanvas;
use crate::canvas::{InputSurface, handle_pointer_controller_event};
use gtk4::{
    EventControllerLegacy, PropagationPhase, Widget, glib, glib::clone, prelude::*,
    subclass::prelude::*,
};
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_compose::ext::AabbExt;
use rnote_compose::penevent::PenState;
use std::cell::{Cell, RefCell};
use tracing::error;

mod imp {
    use super::*;

    /// The magnified view. It has no engine of its own, it draws and writes into the followed canvas.
    #[derive(Debug)]
    pub(crate) struct RnZoomCanvas {
        pub(crate) canvas: glib::WeakRef<RnCanvas>,
        pub(crate) pointer_controller: EventControllerLegacy,
        /// Mirrors the followed canvas cursor, so the pointer looks the same in both views.
        pub(crate) cursor_binding: RefCell<Option<glib::Binding>>,
    }

    impl Default for RnZoomCanvas {
        fn default() -> Self {
            let pointer_controller = EventControllerLegacy::builder()
                .name("zoom_pointer_controller")
                .propagation_phase(PropagationPhase::Bubble)
                .build();

            Self {
                canvas: glib::WeakRef::new(),
                cursor_binding: RefCell::new(None),
                pointer_controller,
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RnZoomCanvas {
        const NAME: &'static str = "RnZoomCanvas";
        type Type = super::RnZoomCanvas;
        type ParentType = Widget;
    }

    impl ObjectImpl for RnZoomCanvas {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.add_css_class("view");
            obj.add_controller(self.pointer_controller.clone());
            self.setup_input();
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RnZoomCanvas {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            let obj = self.obj();

            let Some(canvas) = obj.canvas() else {
                return;
            };
            let _ = canvas
                .engine_mut()
                .zoom_window_set_size(Vector2::new(width as f64, height as f64));
            canvas.queue_draw();
            obj.queue_draw();
        }

        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            let obj = self.obj();

            let Some(canvas) = obj.canvas() else {
                return;
            };
            if let Err(e) = canvas
                .engine_ref()
                .draw_zoom_window_to_gtk_snapshot(snapshot, obj.bounds())
            {
                error!("Snapshot zoom canvas failed, Err: {e:?}");
            }
        }
    }

    impl RnZoomCanvas {
        fn setup_input(&self) {
            let obj = self.obj();

            // Same pipeline as the main canvas, only the target widget and the camera differ.
            let pen_state = Cell::new(PenState::Up);
            self.pointer_controller.connect_event(clone!(
                #[strong]
                pen_state,
                #[weak(rename_to=zoomcanvas)]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, event| {
                    let Some(canvas) = zoomcanvas.canvas() else {
                        return glib::Propagation::Proceed;
                    };
                    let (propagation, new_state) = handle_pointer_controller_event(
                        &canvas,
                        event,
                        pen_state.get(),
                        InputSurface::ZoomWindow(zoomcanvas.upcast_ref()),
                    );
                    pen_state.set(new_state);
                    propagation
                }
            ));
        }
    }
}

glib::wrapper! {
    pub(crate) struct RnZoomCanvas(ObjectSubclass<imp::RnZoomCanvas>)
        @extends Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for RnZoomCanvas {
    fn default() -> Self {
        Self::new()
    }
}

impl RnZoomCanvas {
    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    /// The followed canvas, if it still exists.
    pub(crate) fn canvas(&self) -> Option<RnCanvas> {
        self.imp().canvas.upgrade()
    }

    pub(crate) fn set_canvas(&self, canvas: Option<&RnCanvas>) {
        self.imp().canvas.set(canvas);
        self.sync_visible();

        // The main canvas swaps its cursor between regular, drawing and invisible.
        // Bind it so the same pointer shows here instead of the default arrow.
        if let Some(binding) = self.imp().cursor_binding.take() {
            binding.unbind();
        }

        if let Some(canvas) = canvas {
            let binding = canvas
                .bind_property("cursor", self, "cursor")
                .sync_create()
                .build();
            self.imp().cursor_binding.replace(Some(binding));

            let _ = canvas
                .engine_mut()
                .zoom_window_set_size(self.bounds().extents());
        }
        self.queue_draw();
    }

    /// Tell the followed engine whether the panel is shown, so it draws the box on the main canvas.
    pub(crate) fn sync_visible(&self) {
        let Some(canvas) = self.canvas() else {
            return;
        };
        let visible = self
            .parent()
            .is_some_and(|zoomwindow| zoomwindow.is_visible());
        let _ = canvas.engine_mut().zoom_window_set_visible(visible);
        canvas.queue_draw();
    }

    fn bounds(&self) -> Aabb {
        Aabb::new_positive(
            Vector2::ZERO,
            Vector2::new(self.width() as f64, self.height() as f64),
        )
    }
}
