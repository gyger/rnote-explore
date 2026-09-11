// Modules
mod zoomcanvas;

// Re-exports
pub(crate) use zoomcanvas::RnZoomCanvas;

// Imports
use crate::RnCanvas;
use gtk4::{CompositeTemplate, Widget, glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    /// The zoom window panel: a toolbar and a magnified view of the active canvas.
    #[derive(Default, Debug, CompositeTemplate)]
    #[template(resource = "/com/github/flxzt/rnote/ui/zoomwindow.ui")]
    pub(crate) struct RnZoomWindow {
        #[template_child]
        pub(crate) zoomcanvas: TemplateChild<RnZoomCanvas>,
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
    }
}
