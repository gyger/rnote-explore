//! Playback of slides shown in a document.
//!
//! Kept beside the store, not in the strokes: changing a stroke every frame
//! would copy it and the stroke map, and dirty the undo history. A slide
//! that is not live shows its stored still frame through the normal
//! rendering; a live one is drawn every frame at its clock's time.
//!
//! ```text
//!   still ──play──► playing ──finish/end──► paused at end
//!                     │  ▲                     │
//!                 pause│  │resume          rewind
//!                     ▼  │                     ▼
//!                   paused ◄──────────────── paused at 0
//! ```

use super::SlideSource;
use super::render::SlideImages;
use super::timeline::Slide;
use crate::store::{LiveStrokes, StrokeKey};
use p2d::bounding_volume::Aabb;
use std::collections::HashMap;
use std::time::Instant;

/// Section type substrings with player behavior (slide-format.md, section 9).
const TYPE_LOOP: &str = "loop";
const TYPE_SKIP: &str = "skip";

/// Where a slide is in its playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Position {
    /// Not live: the stored still frame shows.
    Still,
    Playing,
    Start,
    Middle,
    End,
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
struct LiveSlide {
    slide: Slide,
    images: SlideImages,
    clock: Clock,
}

impl LiveSlide {
    fn looping(&self) -> bool {
        self.slide.kind.to_lowercase().contains(TYPE_LOOP)
    }

    fn skipping(&self) -> bool {
        self.slide.kind.to_lowercase().contains(TYPE_SKIP)
    }

    /// Slide time at `now`: clamped to the slide, or wrapped for looping ones.
    fn time_at(&self, now: Instant) -> f64 {
        let duration = self.slide.duration;
        let t = match self.clock {
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
    live: HashMap<StrokeKey, LiveSlide>,
}

impl SlidePlayer {
    pub(crate) fn position(&self, key: StrokeKey) -> Position {
        let Some(live) = self.live.get(&key) else {
            return Position::Still;
        };

        match live.clock {
            Clock::Playing { .. } => Position::Playing,
            Clock::Paused(t) if t <= 0.0 => Position::Start,
            Clock::Paused(t) if t >= live.slide.duration => Position::End,
            Clock::Paused(_) => Position::Middle,
        }
    }

    pub(crate) fn looping(&self, key: StrokeKey) -> bool {
        self.live.get(&key).is_some_and(LiveSlide::looping)
    }

    /// Play `source` from its start, decoding it if it is not live yet.
    pub(crate) fn play(&mut self, key: StrokeKey, source: &SlideSource) -> anyhow::Result<()> {
        let clock = Clock::Playing {
            since: Instant::now(),
            from: 0.0,
        };

        if let Some(live) = self.live.get_mut(&key) {
            live.clock = clock;
            return Ok(());
        }

        let (slide, images) = source.decode()?;
        self.live.insert(
            key,
            LiveSlide {
                slide,
                images,
                clock,
            },
        );
        Ok(())
    }

    /// Go on from where a paused slide stopped.
    pub(crate) fn resume(&mut self, key: StrokeKey) {
        let Some(live) = self.live.get_mut(&key) else {
            return;
        };
        if let Clock::Paused(from) = live.clock {
            live.clock = Clock::Playing {
                since: Instant::now(),
                from,
            };
        }
    }

    /// Jump to the end state.
    pub(crate) fn finish(&mut self, key: StrokeKey) {
        if let Some(live) = self.live.get_mut(&key) {
            live.clock = Clock::Paused(live.slide.duration);
        }
    }

    /// Show the start state.
    pub(crate) fn rewind(&mut self, key: StrokeKey) {
        if let Some(live) = self.live.get_mut(&key) {
            live.clock = Clock::Paused(0.0);
        }
    }

    /// Back to the stored still frame.
    pub(crate) fn stop(&mut self, key: StrokeKey) {
        self.live.remove(&key);
    }

    /// Advance the clocks: slides that reached their end stop there.
    pub(crate) fn frame(&mut self) -> FrameResult {
        let now = Instant::now();
        let mut result = FrameResult::default();

        for live in self.live.values_mut() {
            let Clock::Playing { .. } = live.clock else {
                continue;
            };
            result.redraw = true;

            let t = live.time_at(now);
            if live.looping() || t < live.slide.duration {
                result.playing = true;
                continue;
            }

            live.clock = Clock::Paused(live.slide.duration);
            result.advance |= live.skipping();
        }
        result
    }
}

impl LiveStrokes for SlidePlayer {
    fn is_live(&self, key: StrokeKey) -> bool {
        self.live.contains_key(&key)
    }

    fn draw_live(
        &self,
        key: StrokeKey,
        cx: &mut piet_cairo::CairoRenderContext,
        bounds: Aabb,
    ) -> anyhow::Result<()> {
        let Some(live) = self.live.get(&key) else {
            return Ok(());
        };

        let target = kurbo::Rect::new(
            bounds.mins[0],
            bounds.mins[1],
            bounds.maxs[0],
            bounds.maxs[1],
        );
        live.slide
            .draw_at(cx, &live.images, live.time_at(Instant::now()), target)
    }
}
