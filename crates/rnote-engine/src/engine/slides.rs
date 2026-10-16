//! Manim slide control: one slide per page, stepped like a deck.
//!
//! ```text
//!  forward:  still/start ─► play ─► (playing) finish ─► (end) next page, play its slide
//!  back:     playing/middle/end ─► rewind ─► (start) previous page, its slide at the end
//! ```
//!
//! A clicker sends forward and back, so a lecture runs on two buttons.

// Imports
use crate::WidgetFlags;
use crate::engine::Engine;
use crate::manimslide::Position;
use crate::store::StrokeKey;
use crate::strokes::Stroke;
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_compose::shapes::Shapeable;
use tracing::error;

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

        match self.slides.position(key) {
            Position::Still => self.slide_play(key),
            Position::Start | Position::Middle => {
                self.slides.resume(key);
                self.slide_frames()
            }
            Position::Playing if !self.slides.looping(key) => {
                self.slides.finish(key);
                redraw()
            }
            Position::Playing | Position::End => {
                self.slides.stop(key);
                self.slide_page_step(PageStep::Next)
            }
        }
    }

    /// Rewind the slide in view, or go back a page when it is at its start.
    pub fn slide_back(&mut self) -> WidgetFlags {
        let Some(key) = self.slide_in_view() else {
            return self.slide_page_step(PageStep::Previous);
        };

        match self.slides.position(key) {
            Position::Playing | Position::Middle | Position::End => {
                self.slides.rewind(key);
                redraw()
            }
            Position::Still | Position::Start => {
                self.slides.stop(key);
                self.slide_page_step(PageStep::Previous)
            }
        }
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
        redraw() | self.slide_forward()
    }

    /// The topmost slide under the center of the view.
    fn slide_in_view(&self) -> Option<StrokeKey> {
        let center = self.camera.viewport_center();
        let point = Aabb::new(center, center);

        self.store
            .stroke_keys_as_rendered_intersecting_bounds(point)
            .into_iter()
            .rev()
            .find(|&key| {
                matches!(
                    self.store.get_stroke_ref(key),
                    Some(Stroke::VectorImage(image)) if image.manim_slide.is_some()
                        && image.bounds().contains_local_point(center)
                )
            })
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

    /// Start the frame loop of playing slides.
    fn slide_frames(&mut self) -> WidgetFlags {
        self.animation.claim_frame();
        redraw()
    }

    /// Move the view by one page; a slide on the next page starts playing.
    fn slide_page_step(&mut self, step: PageStep) -> WidgetFlags {
        let page_height = self.document.config.format.height() * self.camera.total_zoom();
        let delta = match step {
            PageStep::Previous => -page_height,
            PageStep::Next => page_height,
        };

        let offset = self.camera.offset() + Vector2::new(0.0, delta);
        let mut widget_flags = self.camera_set_offset_expand(offset);
        if step == PageStep::Next
            && let Some(key) = self.slide_in_view()
        {
            widget_flags |= self.slide_play(key);
        }
        widget_flags
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

    /// An engine with two pages, slides on both, viewing the first.
    fn engine_with_slides() -> (Engine, [StrokeKey; 2]) {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page = engine.document.config.format.size();
        engine.document.height = page[1] * 2.0;

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/manimslide/0000_arcs.slide.json"
        );
        let keys = [0.0, 1.0].map(|index| {
            let source = SlideSource::from_json(std::fs::read_to_string(path).unwrap()).unwrap();
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
    fn forward_plays_finishes_then_turns_the_page() {
        let (mut engine, [first, second]) = engine_with_slides();
        assert_eq!(engine.slide_in_view(), Some(first));

        let _ = engine.slide_forward();
        assert_eq!(engine.slides.position(first), Position::Playing);

        let _ = engine.slide_forward();
        assert_eq!(engine.slides.position(first), Position::End);

        let _ = engine.slide_forward();
        assert_eq!(engine.slides.position(first), Position::Still);
        assert_eq!(engine.slide_in_view(), Some(second));
        assert_eq!(engine.slides.position(second), Position::Playing);
    }

    #[test]
    fn back_rewinds_then_returns_a_page() {
        let (mut engine, [first, second]) = engine_with_slides();
        let _ = engine.slide_forward();
        let _ = engine.slide_forward();
        let _ = engine.slide_forward();

        let _ = engine.slide_back();
        assert_eq!(engine.slides.position(second), Position::Start);

        let _ = engine.slide_back();
        assert_eq!(engine.slides.position(second), Position::Still);
        assert_eq!(engine.slide_in_view(), Some(first));
    }
}
