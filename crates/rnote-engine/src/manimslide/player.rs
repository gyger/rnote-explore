//! Playback of slides shown in a document.
//!
//! Kept beside the store, not in the strokes: changing a stroke every frame
//! would copy it and the stroke map, and dirty the undo history.
//!
//! ```text
//!   unplayed ──play──► playing ──finish/end──► paused at end
//!   (t = 0)              │  ▲                     │
//!                    pause│  │resume          rewind
//!                        ▼  │                     ▼
//!                      paused ◄──────────────── paused at 0
//! ```
//!
//! Each view asks through [SlideView] how to show slides: the audience
//! always sees them live, the lecturer as configured.

use super::SlideSource;
use super::render::SlideImages;
use super::timeline::Slide;
use crate::engine::slides::LecturerSlides;
use crate::store::{LiveStrokes, StrokeKey};
use crate::strokes::Stroke;
use p2d::bounding_volume::Aabb;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Section type substrings with player behavior (slide-format.md, section 9).
const TYPE_LOOP: &str = "loop";
const TYPE_SKIP: &str = "skip";

/// Where a slide is in its playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Position {
    /// Not played yet: shows its start.
    Unplayed,
    Playing,
    Start,
    Middle,
    End,
}

/// A view that draws slides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlideView {
    /// The main canvas and the zoom window.
    Lecturer(LecturerSlides),
    /// The presentation window.
    Audience,
}

/// What one animation frame changed.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameResult {
    /// Some slide still plays, so another frame is needed.
    pub(crate) playing: bool,
    /// A `skip` slide ended and asks to go on to the next one.
    pub(crate) advance: bool,
    /// Some slide played during this frame, so the views changed.
    pub(crate) redraw: bool,
}

#[derive(Debug, Clone, Copy)]
enum Clock {
    Paused(f64),
    Playing { since: Instant, from: f64 },
}

#[derive(Debug)]
struct Decoded {
    /// The slide text this was decoded from; an updated or undone slide has another.
    text: Arc<str>,
    slide: Slide,
    images: SlideImages,
}

impl Decoded {
    fn looping(&self) -> bool {
        self.slide.kind.to_lowercase().contains(TYPE_LOOP)
    }

    fn skipping(&self) -> bool {
        self.slide.kind.to_lowercase().contains(TYPE_SKIP)
    }

    /// Slide time of `clock` at `now`: clamped to the slide, or wrapped for looping ones.
    fn time_at(&self, clock: Clock, now: Instant) -> f64 {
        let duration = self.slide.duration;
        let t = match clock {
            Clock::Paused(t) => t,
            Clock::Playing { since, from } => from + now.duration_since(since).as_secs_f64(),
        };

        if self.looping() && duration > 0.0 {
            return t % duration;
        }
        t.clamp(0.0, duration)
    }
}

#[derive(Debug, Default)]
pub(crate) struct SlidePlayer {
    /// Slides that were played; the others show their start.
    clocks: HashMap<StrokeKey, Clock>,
    /// Decoded on first use, also while drawing.
    decoded: RefCell<HashMap<StrokeKey, Arc<Decoded>>>,
}

/// The slide a stroke shows, if it is one.
fn slide_source(stroke: &Stroke) -> Option<&SlideSource> {
    match stroke {
        Stroke::VectorImage(image) => image.manim_slide.as_ref(),
        _ => None,
    }
}

impl SlidePlayer {
    fn decoded(&self, key: StrokeKey, source: &SlideSource) -> anyhow::Result<Arc<Decoded>> {
        if let Some(decoded) = self.decoded.borrow().get(&key)
            && source.is_same_text(&decoded.text)
        {
            return Ok(Arc::clone(decoded));
        }

        let (slide, images) = source.decode()?;
        let decoded = Arc::new(Decoded {
            text: Arc::clone(&source.slide_json),
            slide,
            images,
        });
        self.decoded.borrow_mut().insert(key, Arc::clone(&decoded));
        Ok(decoded)
    }

    /// The decoded slide of a played slide; it was decoded when it started.
    fn played(&self, key: StrokeKey) -> Option<Arc<Decoded>> {
        self.decoded.borrow().get(&key).cloned()
    }

    pub(crate) fn position(&self, key: StrokeKey) -> Position {
        let (Some(clock), Some(decoded)) = (self.clocks.get(&key), self.played(key)) else {
            return Position::Unplayed;
        };

        match *clock {
            Clock::Playing { .. } => Position::Playing,
            Clock::Paused(t) if t <= 0.0 => Position::Start,
            Clock::Paused(t) if t >= decoded.slide.duration => Position::End,
            Clock::Paused(_) => Position::Middle,
        }
    }

    pub(crate) fn looping(&self, key: StrokeKey) -> bool {
        self.played(key).is_some_and(|d| d.looping())
    }

    /// Play `source` from its start.
    pub(crate) fn play(&mut self, key: StrokeKey, source: &SlideSource) -> anyhow::Result<()> {
        self.decoded(key, source)?;
        let clock = Clock::Playing {
            since: Instant::now(),
            from: 0.0,
        };
        self.clocks.insert(key, clock);
        Ok(())
    }

    /// Show the end state of `source`, e.g. when going back to it.
    pub(crate) fn show_end(&mut self, key: StrokeKey, source: &SlideSource) -> anyhow::Result<()> {
        let duration = self.decoded(key, source)?.slide.duration;
        self.clocks.insert(key, Clock::Paused(duration));
        Ok(())
    }

    /// Go on from where a paused slide stopped.
    pub(crate) fn resume(&mut self, key: StrokeKey) {
        if let Some(clock) = self.clocks.get_mut(&key)
            && let Clock::Paused(from) = *clock
        {
            *clock = Clock::Playing {
                since: Instant::now(),
                from,
            };
        }
    }

    /// Jump a played slide to its end state.
    pub(crate) fn finish(&mut self, key: StrokeKey) {
        let Some(decoded) = self.played(key) else {
            return;
        };
        self.clocks
            .insert(key, Clock::Paused(decoded.slide.duration));
    }

    /// Show the start state of a played slide.
    pub(crate) fn rewind(&mut self, key: StrokeKey) {
        if let Some(clock) = self.clocks.get_mut(&key) {
            *clock = Clock::Paused(0.0);
        }
    }

    /// Forget a slide's playback, e.g. after its slide data changed: it shows its start again.
    pub(crate) fn forget(&mut self, key: StrokeKey) {
        self.clocks.remove(&key);
        self.decoded.borrow_mut().remove(&key);
    }

    /// Advance the clocks: slides that reached their end stop there.
    pub(crate) fn frame(&mut self) -> FrameResult {
        let now = Instant::now();
        let mut result = FrameResult::default();
        let decoded = self.decoded.borrow();

        for (key, clock) in self.clocks.iter_mut() {
            let Clock::Playing { .. } = *clock else {
                continue;
            };
            let Some(decoded) = decoded.get(key) else {
                continue;
            };
            result.redraw = true;

            let duration = decoded.slide.duration;
            if decoded.looping() || decoded.time_at(*clock, now) < duration {
                result.playing = true;
                continue;
            }

            *clock = Clock::Paused(duration);
            result.advance |= decoded.skipping();
        }
        result
    }

    /// How `view` draws slides.
    pub(crate) fn view(&self, view: SlideView) -> ViewSlides<'_> {
        ViewSlides { player: self, view }
    }
}

/// The player as seen by one view.
#[derive(Debug)]
pub(crate) struct ViewSlides<'a> {
    player: &'a SlidePlayer,
    view: SlideView,
}

impl LiveStrokes for ViewSlides<'_> {
    fn is_live(&self, _key: StrokeKey, stroke: &Stroke) -> bool {
        // The lecturer's end state is the stored still frame, drawn like any image.
        slide_source(stroke).is_some() && self.view != SlideView::Lecturer(LecturerSlides::EndState)
    }

    fn draw_live(
        &self,
        key: StrokeKey,
        stroke: &Stroke,
        cx: &mut piet_cairo::CairoRenderContext,
        bounds: Aabb,
    ) -> anyhow::Result<()> {
        let Some(source) = slide_source(stroke) else {
            return Ok(());
        };
        let decoded = self.player.decoded(key, source)?;
        let t = match self.player.clocks.get(&key) {
            Some(clock) => decoded.time_at(*clock, Instant::now()),
            None => 0.0,
        };

        let target = kurbo::Rect::new(
            bounds.mins[0],
            bounds.mins[1],
            bounds.maxs[0],
            bounds.maxs[1],
        );
        decoded.slide.draw_at(cx, &decoded.images, t, target)
    }
}
