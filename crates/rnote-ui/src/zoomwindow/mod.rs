// Modules
mod zoomcanvas;

// Re-exports
pub(crate) use zoomcanvas::RnZoomCanvas;

// Imports
use crate::RnCanvas;
use gtk4::{
    CompositeTemplate, DropDown, SpinButton, ToggleButton, Widget, glib, glib::clone, prelude::*,
    subclass::prelude::*,
};
use rnote_engine::document::background::PatternStyle;
use rnote_engine::zoomwindow::LineStart;

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
        #[template_child]
        pub(crate) line_start_dropdown: TemplateChild<DropDown>,
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
            self.line_start_dropdown.connect_selected_notify(clone!(
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
    /// Redraw the magnified view. GTK caches child render nodes, so the inner canvas must be queued itself.
    pub(crate) fn queue_redraw(&self) {
        self.imp().zoomcanvas.queue_draw();
    }

    pub(crate) fn set_canvas(&self, canvas: Option<&RnCanvas>) {
        self.imp().zoomcanvas.set_canvas(canvas);
        self.default_return_height(canvas);
        self.push_settings();
    }

    /// Ruled or grid paper knows its line spacing; take it as the return height.
    fn default_return_height(&self, canvas: Option<&RnCanvas>) {
        let Some(canvas) = canvas else {
            return;
        };
        let background = canvas.engine_ref().document.config.background;
        if matches!(background.pattern, PatternStyle::None) {
            return;
        }
        self.imp()
            .return_height_spinbutton
            .set_value(background.pattern_size[1]);
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
        engine
            .zoom_window_set_line_start(line_start_from_index(imp.line_start_dropdown.selected()));
        drop(engine);

        canvas.queue_draw();
        imp.zoomcanvas.queue_draw();
    }
}

/// Map the dropdown rows to the engine setting. Row order as in the template.
fn line_start_from_index(index: u32) -> LineStart {
    const LAST_PLACED_ROW: u32 = 1;
    if index == LAST_PLACED_ROW {
        LineStart::LastPlaced
    } else {
        LineStart::PageEdge
    }
}
