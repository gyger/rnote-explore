//! Playback of slides shown in a document.
//!
//! Kept beside the store, not in the strokes: changing a stroke every frame
//! would copy it and the stroke map, and dirty the undo history.
//!
//! A slide plays in parts, split by its click stops (slide-format.md, 9.1):
//!
//! ```text
//!   stops = [1, 2]        part 0    part 1    part 2
//!                       |---------|---------|---------|
//!                       0         1         2      duration
//!
//!   forward: play to the next stop and halt there; while playing, jump to the
//!            part's end (a looped part: end the loop and play on); at the end, leave
//!   back:    halt at the last stop before now; at 0, leave
//! ```
//!
//! Each view asks through [SlideView] how to show slides: the audience
//! always sees them live, the lecturer as configured.

use super::SlideSource;
use super::render::Cues;
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
    /// Halted inside the slide: at a stop or paused.
    Middle,
    End,
}

/// Whether a step stays on the slide or leaves it for the neighbouring page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Stay,
    Leave,
}

/// A view that draws slides; only the views of the ui feature draw them.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
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
    /// Playing the part that contains `from` since `since`.
    Playing {
        since: Instant,
        from: f64,
    },
}

#[derive(Debug)]
struct Decoded {
    /// The take this was decoded from; an updated or undone slide has another.
    take: Arc<str>,
    slide: Arc<Slide>,
}

impl Decoded {
    /// A slide of type `loop` repeats as a whole and never ends.
    fn looping(&self) -> bool {
        self.slide.kind.to_lowercase().contains(TYPE_LOOP)
    }

    fn skipping(&self) -> bool {
        self.slide.kind.to_lowercase().contains(TYPE_SKIP)
    }

    /// Slide time of `clock` at `now`. Playing stays within its part: a looped part
    /// wraps, any other halts at its end.
    fn time_at(&self, clock: Clock, now: Instant) -> f64 {
        let duration = self.slide.duration;
        let (since, from) = match clock {
            Clock::Paused(t) => return t,
            Clock::Playing { since, from } => (since, from),
        };
        let t = from + now.duration_since(since).as_secs_f64();

        if self.looping() && duration > 0.0 {
            return t % duration;
        }
        let part = self.slide.part_at(from);
        let (start, end) = self.slide.part_range(part);
        if self.slide.part_loops(part) && end > start {
            return start + (t - start) % (end - start);
        }
        t.min(end)
    }

    /// The end of the part `clock` plays, or `None` when it never halts.
    fn halting_end(&self, clock: Clock) -> Option<f64> {
        let Clock::Playing { from, .. } = clock else {
            return None;
        };
        let part = self.slide.part_at(from);
        if self.looping() || self.slide.part_loops(part) {
            return None;
        }
        Some(self.slide.part_range(part).1)
    }
}

#[derive(Debug, Default)]
pub(crate) struct SlidePlayer {
    /// Slides that were played; the others show their start.
    clocks: HashMap<StrokeKey, Clock>,
    /// Decoded on first use, also while drawing.
    decoded: RefCell<HashMap<StrokeKey, Arc<Decoded>>>,
}

/// The live slide a stroke shows, if it is one. Notes copies stay still frames.
fn slide_source(stroke: &Stroke) -> Option<&SlideSource> {
    match stroke {
        Stroke::VectorImage(image) => image
            .manim_slide
            .as_ref()
            .filter(|source| !source.is_notes_copy()),
        _ => None,
    }
}

impl SlidePlayer {
    fn decoded(&self, key: StrokeKey, source: &SlideSource) -> anyhow::Result<Arc<Decoded>> {
        if let Some(decoded) = self.decoded.borrow().get(&key)
            && source.is_same_take(&decoded.take)
        {
            return Ok(Arc::clone(decoded));
        }

        let decoded = Arc::new(Decoded {
            slide: source.slide()?,
            take: source
                .take
                .clone()
                .ok_or_else(|| anyhow::anyhow!("a notes copy is not played"))?,
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

    /// Slide time of a played slide at `now`.
    pub(crate) fn time_at(&self, key: StrokeKey, now: Instant) -> Option<f64> {
        let decoded = self.played(key)?;
        Some(decoded.time_at(*self.clocks.get(&key)?, now))
    }

    /// Play `source` from its start.
    pub(crate) fn play(&mut self, key: StrokeKey, source: &SlideSource) -> anyhow::Result<()> {
        self.play_at(key, source, Instant::now())
    }

    pub(crate) fn play_at(
        &mut self,
        key: StrokeKey,
        source: &SlideSource,
        now: Instant,
    ) -> anyhow::Result<()> {
        self.decoded(key, source)?;
        self.clocks.insert(
            key,
            Clock::Playing {
                since: now,
                from: 0.0,
            },
        );
        Ok(())
    }

    /// Show the end state of `source`, e.g. when going back to it.
    pub(crate) fn show_end(&mut self, key: StrokeKey, source: &SlideSource) -> anyhow::Result<()> {
        let duration = self.decoded(key, source)?.slide.duration;
        self.clocks.insert(key, Clock::Paused(duration));
        Ok(())
    }

    /// Step a played slide forward: play to the next stop, finish the part playing, or
    /// leave it once it has ended.
    pub(crate) fn forward(&mut self, key: StrokeKey) -> Step {
        self.forward_at(key, Instant::now())
    }

    pub(crate) fn forward_at(&mut self, key: StrokeKey, now: Instant) -> Step {
        let (Some(clock), Some(decoded)) = (self.clocks.get(&key).copied(), self.played(key))
        else {
            return Step::Leave;
        };
        let slide = &decoded.slide;

        let next = match clock {
            Clock::Paused(t) if t >= slide.duration => return Step::Leave,
            Clock::Paused(t) => Clock::Playing {
                since: now,
                from: t,
            },
            Clock::Playing { from, .. } => {
                let part = slide.part_at(from);
                let end = slide.part_range(part).1;
                let looped = slide.part_loops(part);

                if decoded.looping() || (looped && end >= slide.duration) {
                    return Step::Leave;
                }
                if looped {
                    Clock::Playing {
                        since: now,
                        from: end,
                    }
                } else {
                    Clock::Paused(end)
                }
            }
        };
        self.clocks.insert(key, next);
        Step::Stay
    }

    /// Step a played slide back: halt at the last stop before now, or leave it at its start.
    pub(crate) fn back(&mut self, key: StrokeKey) -> Step {
        self.back_at(key, Instant::now())
    }

    pub(crate) fn back_at(&mut self, key: StrokeKey, now: Instant) -> Step {
        let Some(t) = self.time_at(key, now) else {
            return Step::Leave;
        };
        if t <= 0.0 {
            return Step::Leave;
        }

        let Some(decoded) = self.played(key) else {
            return Step::Leave;
        };
        self.clocks
            .insert(key, Clock::Paused(decoded.slide.bound_before(t)));
        Step::Stay
    }

    /// Hold a playing slide at the frame it shows now.
    pub(crate) fn pause(&mut self, key: StrokeKey) {
        let now = Instant::now();
        if let Some(t) = self.time_at(key, now)
            && let Some(clock @ Clock::Playing { .. }) = self.clocks.get_mut(&key)
        {
            *clock = Clock::Paused(t);
        }
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

    /// Forget a slide's playback, e.g. after its slide data changed: it shows its start again.
    pub(crate) fn forget(&mut self, key: StrokeKey) {
        self.clocks.remove(&key);
        self.decoded.borrow_mut().remove(&key);
    }

    /// Advance the clocks: slides halt at the end of the part they play.
    pub(crate) fn frame(&mut self) -> FrameResult {
        self.frame_at(Instant::now())
    }

    pub(crate) fn frame_at(&mut self, now: Instant) -> FrameResult {
        let mut result = FrameResult::default();
        let decoded = self.decoded.borrow();

        for (key, clock) in self.clocks.iter_mut() {
            let Clock::Playing { since, from } = *clock else {
                continue;
            };
            let Some(decoded) = decoded.get(key) else {
                continue;
            };
            result.redraw = true;

            let Some(end) = decoded.halting_end(*clock) else {
                result.playing = true;
                continue;
            };
            if from + now.duration_since(since).as_secs_f64() < end {
                result.playing = true;
                continue;
            }

            *clock = Clock::Paused(end);
            result.advance |= end >= decoded.slide.duration && decoded.skipping();
        }
        result
    }

    /// How `view` draws slides.
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
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
        let cues = match self.view {
            SlideView::Audience => Cues::Hidden,
            SlideView::Lecturer(_) => source.cues(),
        };
        decoded.slide.draw_at(cx, t, target, cues)
    }
}
