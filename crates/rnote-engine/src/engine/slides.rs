//! Manim slide control: one slide per page, stepped like a deck.
//!
//! ```text
//!  forward:  unplayed ─► play ─► to the next stop ─► … ─► (end) next page, play its slide
//!  back:     to the previous stop ─► … ─► (start) previous page, its slide at the end
//! ```
//!
//! A slide reached by scrolling waits at its start for the first forward;
//! one reached by forward plays at once. A clicker sends forward and back,
//! so a lecture runs on two buttons.

// Imports
use crate::WidgetFlags;
use crate::engine::Engine;
use crate::manimslide::{Position, Step};
use crate::pens::PenStyle;
use crate::store::StrokeKey;
use crate::strokes::Stroke;
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_compose::penevent::{KeyboardKey, ModifierKey};
use rnote_compose::shapes::Shapeable;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::error;

/// Key that steps forward, with Shift back, as in the browser player.
const STEP_KEY: char = ' ';

/// What the lecturer's own screen shows of the slides the audience sees animate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "lecturer_slides")]
pub enum LecturerSlides {
    /// The finished slide, like presenter notes: what is coming and where to write.
    #[default]
    #[serde(rename = "end_state")]
    EndState,
    /// The same animation as the audience.
    #[serde(rename = "animate")]
    Animate,
}

/// Direction of a page step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageStep {
    Previous,
    Next,
}

impl Engine {
    /// Play the slide in view, finish it, or go on to the next page once it ended.
    pub fn slide_forward(&mut self) -> WidgetFlags {
        let Some(key) = self.slide_in_view() else {
            return self.slide_page_step(PageStep::Next);
        };

        if self.slides.position(key) == Position::Unplayed {
            return self.slide_play(key);
        }
        match self.slides.forward(key) {
            Step::Stay => self.slide_frames(),
            Step::Leave => self.slide_page_step(PageStep::Next),
        }
    }

    /// Rewind the slide in view, or go back a page when it is at its start.
    pub fn slide_back(&mut self) -> WidgetFlags {
        let Some(key) = self.slide_in_view() else {
            return self.slide_page_step(PageStep::Previous);
        };

        if self.slides.position(key) == Position::Unplayed {
            return self.slide_page_step(PageStep::Previous);
        }
        match self.slides.back(key) {
            Step::Stay => redraw(),
            Step::Leave => self.slide_page_step(PageStep::Previous),
        }
    }

    /// Hold the slide in view at its current frame, or let a held one play on.
    pub fn slide_pause(&mut self) -> WidgetFlags {
        let Some(key) = self.slide_in_view() else {
            return WidgetFlags::default();
        };

        match self.slides.position(key) {
            Position::Playing => {
                self.slides.pause(key);
                redraw()
            }
            Position::Start | Position::Middle => {
                self.slides.resume(key);
                self.slide_frames()
            }
            Position::Unplayed | Position::End => WidgetFlags::default(),
        }
    }

    /// Whether the slide in view is held, so pausing would let it play on.
    pub fn slide_held(&self) -> bool {
        self.slide_in_view().is_some_and(|key| {
            matches!(
                self.slides.position(key),
                Position::Start | Position::Middle
            )
        })
    }

    /// Step with the Space key: forward, with Shift back. `None` when the key is not for slides.
    ///
    /// The typewriter keeps Space for typing.
    pub fn slide_key(
        &mut self,
        key: &KeyboardKey,
        modifiers: &HashSet<ModifierKey>,
    ) -> Option<WidgetFlags> {
        if *key != KeyboardKey::Unicode(STEP_KEY)
            || self.current_pen_style_w_override() == PenStyle::Typewriter
            || modifiers.contains(&ModifierKey::KeyboardCtrl)
            || modifiers.contains(&ModifierKey::KeyboardAlt)
        {
            return None;
        }

        if modifiers.contains(&ModifierKey::KeyboardShift) {
            return Some(self.slide_back());
        }
        Some(self.slide_forward())
    }

    /// Advance playing slides; call on every animation frame.
    pub fn slides_handle_frame(&mut self) -> WidgetFlags {
        let result = self.slides.frame();
        if result.playing {
            self.animation.claim_frame();
        }

        if !result.redraw {
            return WidgetFlags::default();
        }
        if !result.advance {
            return redraw();
        }
        redraw() | self.slide_page_step(PageStep::Next)
    }

    /// How the main canvas and the zoom window show slides.
    #[cfg(feature = "ui")]
    pub(crate) fn lecturer_slide_view(&self) -> crate::manimslide::SlideView {
        crate::manimslide::SlideView::Lecturer(self.config.read().lecturer_slides)
    }

    /// The live slide of the page row in view: the one under the center, else the
    /// leftmost, e.g. while the lecturer looks at the notes column.
    fn slide_in_view(&self) -> Option<StrokeKey> {
        let center = self.camera.viewport_center();
        let page_height = self.document.config.format.height();
        let top = (center[1] / page_height).floor() * page_height;
        let row = Aabb::new(
            Vector2::new(self.document.x, top),
            Vector2::new(self.document.x + self.document.width, top + page_height),
        );

        let live: Vec<(StrokeKey, Aabb)> = self
            .store
            .stroke_keys_as_rendered_intersecting_bounds(row)
            .into_iter()
            .filter_map(|key| match self.store.get_stroke_ref(key) {
                Some(Stroke::VectorImage(image))
                    if image
                        .manim_slide
                        .as_ref()
                        .is_some_and(|source| !source.is_notes_copy()) =>
                {
                    let bounds = image.bounds();
                    let middle = (bounds.mins[1] + bounds.maxs[1]) * 0.5;
                    (middle >= row.mins[1] && middle < row.maxs[1]).then_some((key, bounds))
                }
                _ => None,
            })
            .collect();

        live.iter()
            .rev()
            .find(|(_, bounds)| bounds.contains_local_point(center))
            .or_else(|| {
                live.iter()
                    .min_by(|(_, a), (_, b)| a.mins[0].total_cmp(&b.mins[0]))
            })
            .map(|(key, _)| *key)
    }

    fn slide_play(&mut self, key: StrokeKey) -> WidgetFlags {
        let Some(Stroke::VectorImage(image)) = self.store.get_stroke_ref(key) else {
            return WidgetFlags::default();
        };
        let Some(source) = image.manim_slide.as_ref() else {
            return WidgetFlags::default();
        };

        if let Err(e) = self.slides.play(key, source) {
            error!("Playing Manim slide failed, Err: {e:?}");
            return WidgetFlags::default();
        }
        self.slide_frames()
    }

    /// Show the end state of the slide in view, as when stepping back onto it.
    fn slide_show_end(&mut self, key: StrokeKey) -> WidgetFlags {
        let Some(Stroke::VectorImage(image)) = self.store.get_stroke_ref(key) else {
            return WidgetFlags::default();
        };
        let Some(source) = image.manim_slide.as_ref() else {
            return WidgetFlags::default();
        };

        if let Err(e) = self.slides.show_end(key, source) {
            error!("Showing Manim slide failed, Err: {e:?}");
        }
        redraw()
    }

    /// Start the frame loop of playing slides.
    fn slide_frames(&mut self) -> WidgetFlags {
        self.animation.claim_frame();
        redraw()
    }

    /// Center the view on the neighbouring page: its slide plays, or shows its end going back.
    ///
    /// Pages tile from the coordinate origin. Example: the center at 1.3 page heights is on
    /// page 1, so `Next` centers page 2 at 2.5 page heights, whatever the scroll position.
    fn slide_page_step(&mut self, step: PageStep) -> WidgetFlags {
        let page_height = self.document.config.format.height();
        let center = self.camera.viewport_center();
        let page = (center[1] / page_height).floor();
        let target = match step {
            PageStep::Previous => page - 1.0,
            PageStep::Next => page + 1.0,
        };

        let new_center = Vector2::new(center[0], (target + 0.5) * page_height);
        let offset = new_center * self.camera.total_zoom() - self.camera.size() * 0.5;
        let widget_flags = self.camera_set_offset_expand(offset);
        let Some(key) = self.slide_in_view() else {
            return widget_flags;
        };

        widget_flags
            | match step {
                PageStep::Next => self.slide_play(key),
                PageStep::Previous => self.slide_show_end(key),
            }
    }
}

fn redraw() -> WidgetFlags {
    WidgetFlags {
        redraw: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manimslide::SlideSource;
    use crate::store::chrono_comp::StrokeLayer;
    use crate::strokes::VectorImage;

    /// A slide without stops: ManimGX creating a square, 1 s.
    fn single_slide() -> SlideSource {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/manimslide/take/single.take"
        );
        let take = std::fs::read(path).unwrap();
        SlideSource::from_take(&take, "Single".to_owned(), String::new()).unwrap()
    }

    /// An engine with two pages, slides on both, viewing the first.
    fn engine_with_slides() -> (Engine, [StrokeKey; 2]) {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page = engine.document.config.format.size();
        engine.document.height = page[1] * 2.0;

        let keys = [0.0, 1.0].map(|index| {
            let source = single_slide();
            let top = Vector2::new(0.0, page[1] * index);
            let image = VectorImage::from_manim_slide(source, Aabb::new(top, top + page)).unwrap();
            engine
                .store
                .insert_stroke(Stroke::VectorImage(image), Some(StrokeLayer::Document))
        });

        let _ = engine.camera.set_viewport_center(page * 0.5);
        (engine, keys)
    }

    #[test]
    fn forward_plays_finishes_then_plays_the_next_page() {
        let (mut engine, [first, second]) = engine_with_slides();
        assert_eq!(engine.slide_in_view(), Some(first));
        assert_eq!(engine.slides.position(first), Position::Unplayed);

        let _ = engine.slide_forward();
        assert_eq!(engine.slides.position(first), Position::Playing);

        let _ = engine.slide_forward();
        assert_eq!(engine.slides.position(first), Position::End);

        let _ = engine.slide_forward();
        assert_eq!(engine.slides.position(first), Position::End);
        assert_eq!(engine.slide_in_view(), Some(second));
        assert_eq!(engine.slides.position(second), Position::Playing);
    }

    #[test]
    fn back_rewinds_then_returns_to_the_previous_end() {
        let (mut engine, [first, second]) = engine_with_slides();
        let _ = engine.slide_forward();
        let _ = engine.slide_forward();
        let _ = engine.slide_forward();

        let _ = engine.slide_back();
        assert_eq!(engine.slides.position(second), Position::Start);

        let _ = engine.slide_back();
        assert_eq!(engine.slide_in_view(), Some(first));
        assert_eq!(engine.slides.position(first), Position::End);
    }

    #[test]
    fn page_steps_center_the_neighbouring_page() {
        let (mut engine, [first, second]) = engine_with_slides();
        let page = engine.document.config.format.size();

        // Scrolled a little off the first page's center, still on it.
        let _ = engine
            .camera
            .set_viewport_center(Vector2::new(page[0] * 0.5, page[1] * 0.3));
        let _ = engine.slide_page_step(PageStep::Next);
        assert_eq!(engine.slide_in_view(), Some(second));
        assert!((engine.camera.viewport_center()[1] - page[1] * 1.5).abs() < 1e-6);

        let _ = engine.slide_page_step(PageStep::Previous);
        assert_eq!(engine.slide_in_view(), Some(first));
        assert!((engine.camera.viewport_center()[1] - page[1] * 0.5).abs() < 1e-6);
    }

    #[test]
    fn pause_holds_the_slide_until_resumed() {
        let (mut engine, [first, _]) = engine_with_slides();
        let _ = engine.slide_forward();
        assert_eq!(engine.slides.position(first), Position::Playing);

        assert!(!engine.slide_held());
        let _ = engine.slide_pause();
        assert!(engine.slide_held());
        assert!(matches!(
            engine.slides.position(first),
            Position::Start | Position::Middle
        ));

        let _ = engine.slide_pause();
        assert_eq!(engine.slides.position(first), Position::Playing);
        assert!(!engine.slide_held());
    }

    #[test]
    fn notes_copies_stay_still_and_steps_go_by_row() {
        let (mut engine, [first, second]) = engine_with_slides();
        let page = engine.document.config.format.size();
        engine.document.width = page[0] * 2.0;

        let source = single_slide().into_notes_copy();
        let left = Vector2::new(page[0], 0.0);
        let image = VectorImage::from_manim_slide(source, Aabb::new(left, left + page)).unwrap();
        let copy = engine
            .store
            .insert_stroke(Stroke::VectorImage(image), Some(StrokeLayer::Document));

        // Looking at the copy in column 2 steps the live slide of the row.
        let _ = engine
            .camera
            .set_viewport_center(Vector2::new(page[0] * 1.5, page[1] * 0.5));
        assert_eq!(engine.slide_in_view(), Some(first));
        let _ = engine.slide_forward();
        assert_eq!(engine.slides.position(first), Position::Playing);
        assert_eq!(engine.slides.position(copy), Position::Unplayed);

        // Stepping keeps the column: the next row's live slide plays.
        let _ = engine.slide_forward();
        let _ = engine.slide_forward();
        assert_eq!(engine.slides.position(second), Position::Playing);
        assert!(engine.camera.viewport_center()[0] > page[0]);
    }

    #[test]
    fn space_steps_except_while_typing() {
        let (mut engine, [first, _]) = engine_with_slides();
        let space = KeyboardKey::Unicode(STEP_KEY);

        assert!(engine.slide_key(&space, &HashSet::new()).is_some());
        assert_eq!(engine.slides.position(first), Position::Playing);

        let _ = engine.change_pen_style(PenStyle::Typewriter);
        assert!(engine.slide_key(&space, &HashSet::new()).is_none());
    }
}
