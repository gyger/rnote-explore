// Modules
mod zoomcanvas;

// Re-exports
pub(crate) use zoomcanvas::RnZoomCanvas;

// Imports
use crate::RnCanvas;
use gtk4::{
    CompositeTemplate, SpinButton, ToggleButton, Widget, glib, glib::clone, prelude::*,
    subclass::prelude::*,
};

mod imp {
    use super::*;

    /// The zoom window panel: a toolbar and a magnified view of the active canvas.
    #[derive(Default, Debug, CompositeTemplate)]
    #[template(resource = "/com/github/flxzt/rnote/ui/zoomwindow.ui")]
    pub(crate) struct RnZoomWindow {
        #[template_child]
        pub(crate) zoomcanvas: TemplateChild<RnZoomCanvas>,
        #[template_child]
        pub(crate) auto_advance_toggle: TemplateChild<ToggleButton>,
        #[template_child]
        pub(crate) return_height_spinbutton: TemplateChild<SpinButton>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RnZoomWindow {
        const NAME: &'static str = "RnZoomWindow";
        type Type = super::RnZoomWindow;
        type ParentType = Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for RnZoomWindow {
        fn constructed(&self) {
            self.parent_constructed();

            // The engine draws the box on the main canvas only while the panel is shown.
            self.obj().connect_visible_notify(|zoomwindow| {
                zoomwindow.imp().zoomcanvas.sync_visible();
            });

            // The panel controls are the source of truth; every followed engine gets them pushed.
            self.auto_advance_toggle.connect_toggled(clone!(
                #[weak(rename_to=zoomwindow)]
                self.obj(),
                move |_| zoomwindow.push_settings()
            ));
            self.return_height_spinbutton.connect_value_changed(clone!(
                #[weak(rename_to=zoomwindow)]
                self.obj(),
                move |_| zoomwindow.push_settings()
            ));
        }

        fn dispose(&self) {
            self.dispose_template();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RnZoomWindow {}
}

glib::wrapper! {
    pub(crate) struct RnZoomWindow(ObjectSubclass<imp::RnZoomWindow>)
        @extends Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for RnZoomWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl RnZoomWindow {
    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    /// Follow `canvas`. Called when the active tab changes.
    pub(crate) fn set_canvas(&self, canvas: Option<&RnCanvas>) {
        self.imp().zoomcanvas.set_canvas(canvas);
        self.push_settings();
    }

    /// Push auto-advance and return height into the followed engine.
    fn push_settings(&self) {
        let imp = self.imp();
        let Some(canvas) = imp.zoomcanvas.canvas() else {
            return;
        };

        let mut engine = canvas.engine_mut();
        let _ = engine.zoom_window_set_auto_advance(imp.auto_advance_toggle.is_active());
        engine.zoom_window_set_return_height(imp.return_height_spinbutton.value());
        drop(engine);

        canvas.queue_draw();
        imp.zoomcanvas.queue_draw();
    }
}
