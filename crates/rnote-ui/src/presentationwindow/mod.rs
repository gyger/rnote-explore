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

    /// Show the window, fullscreen on the audience monitor.
    ///
    /// With a single monitor it stays a plain window, so the feature is usable without a
    /// projector - and then the window itself takes the page shape, so nothing is letterboxed.
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

    /// Move the window to the next monitor, wrapping.
    ///
    /// The projector is not always the display the system reports it to be, so the automatic
    /// choice needs an override. Every monitor is offered, the lecturer's included: with only
    /// two displays, skipping it would leave nothing to flip to.
    pub(crate) fn flip_screen(&self, lecturer: &impl IsA<gtk4::Window>) {
        let monitors = monitors(lecturer);
        let current = self
            .imp()
            .monitor_choice
            .get()
            .or_else(|| Some(default_monitor_index(lecturer, &monitors)));

        let Some(next) = next_monitor_choice(current, monitors.len()) else {
            return;
        };
        self.imp().monitor_choice.set(Some(next));

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

/// The monitor to flip to, `None` when there is nothing to flip between.
///
/// A single monitor is the lecturer's own. Flipping onto it would fullscreen the audience view
/// over the lecturer's work, which is worse than doing nothing.
fn next_monitor_choice(current: Option<usize>, monitor_count: usize) -> Option<usize> {
    if monitor_count < 2 {
        return None;
    }

    Some((current.unwrap_or(0) + 1) % monitor_count)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_monitor_has_nothing_to_flip_to() {
        assert_eq!(next_monitor_choice(None, 1), None);
        assert_eq!(next_monitor_choice(Some(0), 1), None);
        assert_eq!(next_monitor_choice(None, 0), None);
    }

    #[test]
    fn flipping_cycles_every_monitor() {
        // The lecturer's own monitor is included, or two displays would have nothing to offer.
        assert_eq!(next_monitor_choice(Some(0), 2), Some(1));
        assert_eq!(next_monitor_choice(Some(1), 2), Some(0));
        assert_eq!(next_monitor_choice(Some(1), 3), Some(2));
        assert_eq!(next_monitor_choice(Some(2), 3), Some(0));
    }
}
