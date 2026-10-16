// Modules
mod presentationcanvas;

// Re-exports
pub(crate) use presentationcanvas::RnPresentationCanvas;

// Imports
use crate::RnCanvas;
use adw::{prelude::*, subclass::prelude::*};
use gettextrs::gettext;
use gtk4::{gdk, glib, glib::clone};
use rnote_engine::engine::slides::SlideControl;
use rnote_engine::presentation::AudienceColumn;
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
        /// The audience stays on the first page column; notes sit in the second.
        pub(crate) first_column: Cell<bool>,
        /// Slide controls in the bottom left corner, shown while the pointer is near.
        pub(crate) slide_controls: gtk4::Revealer,
        /// Steps while nothing runs, pauses a running animation, plays a paused one on.
        pub(crate) slide_center_button: gtk4::Button,
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
            let overlay = gtk4::Overlay::new();
            overlay.set_child(Some(&self.presentationcanvas));
            self.setup_slide_controls(&overlay);
            obj.set_content(Some(&overlay));

            // Space steps through slides here too; the window runs the main window's actions.
            let keys = gtk4::EventControllerKey::new();
            keys.connect_key_pressed(clone!(
                #[weak]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, key, _, modifiers| {
                    let typed = modifiers
                        .intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK);
                    if key != gdk::Key::space || typed {
                        return glib::Propagation::Proceed;
                    }

                    let action = if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                        "win.slide-back"
                    } else {
                        "win.slide-forward"
                    };
                    let _ = obj.activate_action(action, None);
                    glib::Propagation::Stop
                }
            ));
            obj.add_controller(keys);
        }
    }

    impl WidgetImpl for RnPresentationWindow {}
    impl WindowImpl for RnPresentationWindow {}
    impl AdwWindowImpl for RnPresentationWindow {}

    impl RnPresentationWindow {
        /// Back, center and forward in the bottom left corner, for a lecturer standing at
        /// the projector. Hidden from the audience until the pointer comes near; the center
        /// button steps, or pauses and plays on an animation (see [SlideControl]):
        ///
        /// ```text
        ///  ┌──────────────────────────────────┐
        ///  │                                  │
        ///  ├─ near ──────┐                    │
        ///  │ [◀][▶|⏸][▶] │                    │
        ///  └─────────────┴────────────────────┘
        /// ```
        fn setup_slide_controls(&self, overlay: &gtk4::Overlay) {
            let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, CONTROLS_SPACING);
            bar.add_css_class("osd");
            bar.set_opacity(CONTROLS_OPACITY);
            // The canvas hides the system cursor behind its hover marker; buttons need it.
            bar.set_cursor_from_name(Some("default"));

            let back = slide_button(BACK_ICON, BACK_ACTION);
            back.set_tooltip_text(Some(&gettext("Slide Back")));
            let center = &self.slide_center_button;
            setup_slide_button(center);
            let forward = slide_button(FORWARD_ICON, FORWARD_ACTION);
            forward.set_tooltip_text(Some(&gettext("Slide Forward")));
            bar.append(&back);
            bar.append(center);
            bar.append(&forward);
            self.obj().set_slide_control(SlideControl::Next);

            // Pen or laser pointer; GTK highlights the one of the current pen style.
            bar.append(&gtk4::Separator::new(gtk4::Orientation::Vertical));
            let pen = pen_style_button(PEN_ICON, PEN_STYLE_BRUSH);
            pen.set_tooltip_text(Some(&gettext("Pen")));
            let laser = pen_style_button(LASER_ICON, PEN_STYLE_TOOLS);
            laser.set_tooltip_text(Some(&gettext("Laser Pointer")));
            // The tools pen may hold another tool, e.g. zoom: pick the laser too.
            laser.connect_clicked(|laser| {
                let _ = laser.activate_action(LASER_TOOL_ACTION, None);
            });
            bar.append(&pen);
            bar.append(&laser);

            let controls = &self.slide_controls;
            controls.set_child(Some(&bar));
            controls.set_transition_type(gtk4::RevealerTransitionType::Crossfade);
            controls.set_halign(gtk4::Align::Start);
            controls.set_valign(gtk4::Align::End);
            controls.set_margin_start(CONTROLS_MARGIN);
            controls.set_margin_bottom(CONTROLS_MARGIN);
            show_controls(controls, false);
            overlay.add_overlay(controls);

            // Shown while the pointer is in the corner, hidden when it leaves it.
            let motion = gtk4::EventControllerMotion::new();
            motion.connect_motion(clone!(
                #[weak]
                overlay,
                #[weak]
                controls,
                move |_, x, y| {
                    let near = x < NEAR_WIDTH && y > overlay.height() as f64 - NEAR_HEIGHT;
                    show_controls(&controls, near);
                }
            ));
            motion.connect_leave(clone!(
                #[weak]
                controls,
                move |_| show_controls(&controls, false)
            ));
            overlay.add_controller(motion);
        }
    }

    /// The corner where the pointer shows the slide controls, in pixels.
    const NEAR_WIDTH: f64 = 300.0;
    const NEAR_HEIGHT: f64 = 160.0;
    const CONTROLS_MARGIN: i32 = 12;
    const CONTROLS_SPACING: i32 = 0;
    /// Quiet: the audience looks at the slide, not at the controls.
    const CONTROLS_OPACITY: f64 = 0.6;

    /// Show or hide the slide controls. Hidden, they take no input: a stylus writing in the
    /// corner reaches the canvas.
    fn show_controls(controls: &gtk4::Revealer, shown: bool) {
        controls.set_reveal_child(shown);
        controls.set_can_target(shown);
    }

    /// A toggle for pen style `style`, active while it is the current one.
    fn pen_style_button(icon: &str, style: &str) -> gtk4::ToggleButton {
        let button = gtk4::ToggleButton::new();
        button.set_icon_name(icon);
        button.set_action_name(Some(PEN_STYLE_ACTION));
        button.set_action_target_value(Some(&style.to_variant()));
        setup_slide_button(button.upcast_ref());
        button
    }

    fn slide_button(icon: &str, action: &str) -> gtk4::Button {
        let button = gtk4::Button::from_icon_name(icon);
        button.set_action_name(Some(action));
        setup_slide_button(&button);
        button
    }

    fn setup_slide_button(button: &gtk4::Button) {
        button.add_css_class("flat");
        // Never focused: Space must keep stepping forward, not press the last button clicked.
        button.set_focusable(false);
        button.set_focus_on_click(false);
    }
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

/// Slide control icons and the main window's actions they run.
const BACK_ICON: &str = "slide-back-symbolic";
const FORWARD_ICON: &str = "slide-forward-symbolic";
const PAUSE_ICON: &str = "slide-pause-symbolic";
const PLAY_ICON: &str = "slide-play-symbolic";
const BACK_ACTION: &str = "win.slide-back";
const FORWARD_ACTION: &str = "win.slide-forward";
const PAUSE_ACTION: &str = "win.slide-pause";

/// Pen style controls: the main window's pen style action and its targets.
const PEN_ICON: &str = "pen-brush-symbolic";
const LASER_ICON: &str = "pen-tools-laser-symbolic";
const PEN_STYLE_ACTION: &str = "win.pen-style";
const PEN_STYLE_BRUSH: &str = "brush";
const PEN_STYLE_TOOLS: &str = "tools";
const LASER_TOOL_ACTION: &str = "win.laser-tool";

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

    /// Let the center control step, pause or play on, as the slide in view needs.
    pub(crate) fn set_slide_control(&self, control: SlideControl) {
        let (icon, action, tooltip) = match control {
            SlideControl::Next => (FORWARD_ICON, FORWARD_ACTION, gettext("Slide Forward")),
            SlideControl::Pause => (PAUSE_ICON, PAUSE_ACTION, gettext("Pause Slide")),
            SlideControl::Play => (PLAY_ICON, PAUSE_ACTION, gettext("Play Slide")),
        };

        // Called on every redraw while a slide plays; only touch the button on a change.
        let button = &self.imp().slide_center_button;
        if button.icon_name().as_deref() == Some(icon) {
            return;
        }
        button.set_icon_name(icon);
        button.set_action_name(Some(action));
        button.set_tooltip_text(Some(&tooltip));
    }

    /// Hold the audience on the first page column of the lecturer's row.
    pub(crate) fn set_first_column(&self, first_column: bool) {
        self.imp().first_column.set(first_column);
        self.push_state();
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
        let column = if imp.first_column.get() {
            AudienceColumn::First
        } else {
            AudienceColumn::Follow
        };
        let _ = engine.presentation_set_audience_column(column);
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
