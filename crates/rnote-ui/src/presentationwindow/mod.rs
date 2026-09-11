// Modules
mod presentationcanvas;

// Re-exports
pub(crate) use presentationcanvas::RnPresentationCanvas;

// Imports
use crate::RnCanvas;
use adw::{prelude::*, subclass::prelude::*};
use gettextrs::gettext;
use gtk4::{gdk, glib};

mod imp {
    use super::*;

    /// The audience window: the followed canvas, whole page, read-only.
    #[derive(Debug, Default)]
    pub(crate) struct RnPresentationWindow {
        pub(crate) presentationcanvas: RnPresentationCanvas,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RnPresentationWindow {
        const NAME: &'static str = "RnPresentationWindow";
        type Type = super::RnPresentationWindow;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for RnPresentationWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.set_title(Some(&gettext("Rnote - Presentation")));
            obj.set_default_size(
                super::RnPresentationWindow::DEFAULT_WIDTH,
                super::RnPresentationWindow::DEFAULT_HEIGHT,
            );
            // Closing the window only hides it, the toggle owns its lifetime.
            obj.set_hide_on_close(true);
            obj.set_content(Some(&self.presentationcanvas));
        }
    }

    impl WidgetImpl for RnPresentationWindow {}
    impl WindowImpl for RnPresentationWindow {}
    impl AdwWindowImpl for RnPresentationWindow {}
}

glib::wrapper! {
    pub(crate) struct RnPresentationWindow(ObjectSubclass<imp::RnPresentationWindow>)
        @extends adw::Window, gtk4::Window, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget, gtk4::Native,
            gtk4::Root, gtk4::ShortcutManager;
}

impl Default for RnPresentationWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl RnPresentationWindow {
    const DEFAULT_WIDTH: i32 = 1280;
    const DEFAULT_HEIGHT: i32 = 720;

    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    /// Follow `canvas`. Called when the active tab changes.
    pub(crate) fn set_canvas(&self, canvas: Option<&RnCanvas>) {
        self.imp().presentationcanvas.set_canvas(canvas);
    }

    /// Redraw the audience view. GTK caches child render nodes, so the inner canvas must be
    /// queued itself.
    pub(crate) fn queue_redraw(&self) {
        self.imp().presentationcanvas.queue_draw();
    }

    /// Show the window, fullscreen on the monitor the lecturer is not on.
    ///
    /// With a single monitor it stays a plain window, so the feature is usable without a
    /// projector - and then the window itself takes the page shape, so nothing is letterboxed.
    pub(crate) fn present_on_audience_monitor(&self, lecturer: &impl IsA<gtk4::Window>) {
        match audience_monitor(lecturer) {
            Some(monitor) => self.fullscreen_on_monitor(&monitor),
            None => {
                // The projector may be gone since the last time it was shown.
                self.unfullscreen();
                self.resize_to_page_ratio();
            }
        }
        self.present();
    }

    /// Give the window the aspect ratio of the followed page, keeping the default width.
    fn resize_to_page_ratio(&self) {
        let Some(canvas) = self.imp().presentationcanvas.canvas() else {
            return;
        };
        let page_size = canvas.engine_ref().document.config.format.size();
        if page_size[0] <= 0.0 || page_size[1] <= 0.0 {
            return;
        }

        let height = (Self::DEFAULT_WIDTH as f64 * page_size[1] / page_size[0]).round() as i32;
        self.set_default_size(Self::DEFAULT_WIDTH, height);
    }
}

/// The first monitor that is not the one showing `lecturer`.
fn audience_monitor(lecturer: &impl IsA<gtk4::Window>) -> Option<gdk::Monitor> {
    let display = WidgetExt::display(lecturer.as_ref());
    let lecturer_monitor = lecturer
        .as_ref()
        .surface()
        .and_then(|surface| display.monitor_at_surface(&surface));

    display
        .monitors()
        .into_iter()
        .flatten()
        .filter_map(|monitor| monitor.downcast::<gdk::Monitor>().ok())
        .find(|monitor| {
            lecturer_monitor
                .as_ref()
                .is_none_or(|lecturer_monitor| monitor != lecturer_monitor)
        })
}
