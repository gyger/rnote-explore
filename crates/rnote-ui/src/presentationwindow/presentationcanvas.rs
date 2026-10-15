// Imports
use crate::RnCanvas;
use crate::canvas::{InputSurface, handle_pointer_controller_event};
use gtk4::{
    EventControllerLegacy, PropagationPhase, Widget, gdk, glib, glib::clone, graphene, prelude::*,
    subclass::prelude::*,
};
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_compose::ext::AabbExt;
use rnote_compose::penevent::PenState;
use rnote_engine::ext::GrapheneRectExt;
use std::cell::{Cell, RefCell};
use tracing::error;

mod imp {
    use super::*;

    /// The audience view: the followed canvas through the audience camera. Takes stylus input,
    /// for a screenless tablet mapped to the projector.
    #[derive(Debug)]
    pub(crate) struct RnPresentationCanvas {
        pub(crate) canvas: glib::WeakRef<RnCanvas>,
        /// What the audience saw when the view was frozen.
        pub(crate) frozen_still: RefCell<Option<gdk::Texture>>,
        pub(crate) pointer_controller: EventControllerLegacy,
    }

    impl Default for RnPresentationCanvas {
        fn default() -> Self {
            let pointer_controller = EventControllerLegacy::builder()
                .name("presentation_pointer_controller")
                .propagation_phase(PropagationPhase::Bubble)
                .build();

            Self {
                canvas: glib::WeakRef::new(),
                frozen_still: RefCell::new(None),
                pointer_controller,
            }
        }
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
            // The hover marker shows the pen; the system cursor would be a second one.
            obj.set_cursor_from_name(Some("none"));
            obj.add_controller(self.pointer_controller.clone());
            self.setup_input();
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

            if let Some(still) = self.frozen_still.borrow().as_ref() {
                still.snapshot(snapshot, obj.width() as f64, obj.height() as f64);
                return;
            }

            let Some(canvas) = obj.canvas() else {
                return;
            };
            if let Err(e) = canvas
                .engine_ref()
                .draw_audience_to_gtk_snapshot(snapshot, obj.bounds())
            {
                error!("Snapshot presentation canvas failed, Err: {e:?}");
            }
        }
    }

    impl RnPresentationCanvas {
        fn setup_input(&self) {
            let obj = self.obj();

            // Same pipeline as the main canvas, only the target widget and the camera differ.
            let pen_state = Cell::new(PenState::Up);
            let stylus_active = Cell::new(false);
            self.pointer_controller.connect_event(clone!(
                #[weak(rename_to=presentationcanvas)]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, event| {
                    // Stylus only: a stray mouse click on the projector must not write.
                    // A frozen still would hide the ink.
                    let frozen = presentationcanvas.imp().frozen_still.borrow().is_some();
                    if frozen || event.device_tool().is_none() {
                        return glib::Propagation::Proceed;
                    }
                    let Some(canvas) = presentationcanvas.canvas() else {
                        return glib::Propagation::Proceed;
                    };

                    let (propagation, new_state, new_stylus_active) =
                        handle_pointer_controller_event(
                            &canvas,
                            event,
                            pen_state.get(),
                            stylus_active.get(),
                            InputSurface::Audience(presentationcanvas.upcast_ref()),
                        );
                    pen_state.set(new_state);
                    stylus_active.set(new_stylus_active);

                    // The pen left the tablet: drop the hover marker.
                    if event.event_type() == gdk::EventType::ProximityOut {
                        let _ = canvas.engine_mut().tablet_clear();
                        canvas.queue_draw();
                        presentationcanvas.queue_draw();
                    }
                    propagation
                }
            ));
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

    /// Follow `canvas`, `None` while the window is hidden. Only the followed engine is visible.
    pub(crate) fn set_canvas(&self, canvas: Option<&RnCanvas>) {
        if let Some(previous) = self.canvas() {
            let _ = previous.engine_mut().presentation_set_visible(false);
        }
        self.imp().canvas.set(canvas);

        if let Some(canvas) = canvas {
            let mut engine = canvas.engine_mut();
            let _ =
                engine.presentation_set_size(self.bounds().extents(), self.scale_factor() as f64);
            let _ = engine.presentation_set_visible(true);
        }
        self.queue_draw();
    }

    /// Hold the audience on a still of what they see now, or let the view live again.
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

        // Through the engine, not the widget, which would recurse into the frozen branch.
        let snapshot = gtk4::Snapshot::new();
        if let Err(e) = canvas
            .engine_ref()
            .draw_audience_to_gtk_snapshot(&snapshot, bounds)
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
