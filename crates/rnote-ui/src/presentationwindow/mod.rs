// Modules
mod presentationcanvas;

// Re-exports
pub(crate) use presentationcanvas::RnPresentationCanvas;

// Imports
use crate::RnCanvas;
use adw::{prelude::*, subclass::prelude::*};
use gettextrs::gettext;
use gtk4::{gdk, glib};
use std::cell::Cell;

mod imp {
    use super::*;

    /// The audience window: the followed canvas, whole page, read-only.
    #[derive(Debug, Default)]
    pub(crate) struct RnPresentationWindow {
        pub(crate) presentationcanvas: RnPresentationCanvas,
        /// The monitor picked by flipping, as an index into the display's monitor list.
        /// `None` picks the first monitor that is not the lecturer's.
        pub(crate) monitor_choice: Cell<Option<usize>>,
        /// Lock and blank also live in the engine; kept here for the next followed canvas.
        pub(crate) page_locked: Cell<bool>,
        pub(crate) blanked: Cell<bool>,
        pub(crate) frozen: Cell<bool>,
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
        self.push_state();
    }

    /// Hold the audience on the page they are seeing. Ink on it still appears.
    pub(crate) fn set_page_locked(&self, locked: bool) {
        self.imp().page_locked.set(locked);
        self.push_state();
    }

    /// Hold the audience on a still of what they see now.
    pub(crate) fn set_frozen(&self, frozen: bool) {
        self.imp().frozen.set(frozen);
        self.imp().presentationcanvas.set_frozen(frozen);
    }

    /// Show the audience nothing but the letterbox.
    pub(crate) fn set_blanked(&self, blanked: bool) {
        self.imp().blanked.set(blanked);
        self.push_state();
    }

    /// Push lock and blank onto the followed canvas.
    fn push_state(&self) {
        let imp = self.imp();
        let Some(canvas) = imp.presentationcanvas.canvas() else {
            return;
        };

        let mut engine = canvas.engine_mut();
        let _ = engine.presentation_set_page_locked(imp.page_locked.get());
        let _ = engine.presentation_set_blanked(imp.blanked.get());
        drop(engine);

        self.queue_redraw();
    }

    /// Re-shape a windowed audience view after the page format changed. Fullscreen letterboxes.
    pub(crate) fn refresh_page_ratio(&self) {
        if self.is_fullscreen() {
            return;
        }
        self.resize_to_page_ratio();
    }

    /// Redraw the audience view. GTK caches child render nodes, so queue the inner canvas.
    pub(crate) fn queue_redraw(&self) {
        self.imp().presentationcanvas.queue_draw();
    }

    /// Show the window fullscreen on the audience monitor, or as a page-shaped plain window
    /// when there is none.
    pub(crate) fn present_on_audience_monitor(&self, lecturer: &impl IsA<gtk4::Window>) {
        match self.audience_monitor(lecturer) {
            Some(monitor) => self.fullscreen_on_monitor(&monitor),
            None => {
                // The projector may be gone since the last time it was shown.
                self.unfullscreen();
                self.resize_to_page_ratio();
            }
        }
        self.present();
    }

    /// Move the window to the next monitor, the lecturer's included: with two displays,
    /// skipping it would leave nothing to flip to.
    pub(crate) fn flip_screen(&self, lecturer: &impl IsA<gtk4::Window>) {
        let monitors = monitors(lecturer);

        // A single monitor is the lecturer's own; flipping onto it would cover their work.
        if monitors.len() < 2 {
            return;
        }

        let current = self
            .imp()
            .monitor_choice
            .get()
            .unwrap_or_else(|| default_monitor_index(lecturer, &monitors));
        self.imp()
            .monitor_choice
            .set(Some((current + 1) % monitors.len()));

        self.present_on_audience_monitor(lecturer);
    }

    /// The monitor to show on: the flipped choice, else the first that is not the lecturer's.
    fn audience_monitor(&self, lecturer: &impl IsA<gtk4::Window>) -> Option<gdk::Monitor> {
        let monitors = monitors(lecturer);
        if monitors.is_empty() {
            return None;
        }

        match self.imp().monitor_choice.get() {
            Some(choice) => monitors.get(choice % monitors.len()).cloned(),
            // A single monitor is the lecturer's own, so there is no audience monitor.
            None => {
                let index = default_monitor_index(lecturer, &monitors);
                (monitors.len() > 1).then(|| monitors[index].clone())
            }
        }
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

/// Every monitor of the display showing `lecturer`, in the order the display reports them.
fn monitors(lecturer: &impl IsA<gtk4::Window>) -> Vec<gdk::Monitor> {
    WidgetExt::display(lecturer.as_ref())
        .monitors()
        .into_iter()
        .flatten()
        .filter_map(|monitor| monitor.downcast::<gdk::Monitor>().ok())
        .collect()
}

/// Index of the first monitor that is not the one showing `lecturer`, 0 if there is none.
fn default_monitor_index(lecturer: &impl IsA<gtk4::Window>, monitors: &[gdk::Monitor]) -> usize {
    let display = WidgetExt::display(lecturer.as_ref());
    let lecturer_monitor = lecturer
        .as_ref()
        .surface()
        .and_then(|surface| display.monitor_at_surface(&surface));

    monitors
        .iter()
        .position(|monitor| {
            lecturer_monitor
                .as_ref()
                .is_none_or(|lecturer_monitor| monitor != lecturer_monitor)
        })
        .unwrap_or(0)
}
