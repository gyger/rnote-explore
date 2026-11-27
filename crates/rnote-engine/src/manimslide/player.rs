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
use crate::store::{DrawJob, LiveStrokes, StrokeKey};
use crate::strokes::Stroke;
use p2d::bounding_volume::Aabb;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::Instant;
use tracing::error;

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

/// How decoding a slide that waits to play ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decoding {
    Done,
    /// Logged by the decoder; the slide does not play.
    Failed,
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

/// Decoded slides, shared with the thread that draws still frames.
type Slides = Mutex<HashMap<StrokeKey, Arc<Decoded>>>;

fn locked(slides: &Slides) -> MutexGuard<'_, HashMap<StrokeKey, Arc<Decoded>>> {
    // The map holds finished slides only, so it is whole even after a panic.
    slides.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The slide of `source`, decoded on first use. Decoding is the slow part: `slides` is
/// not locked meanwhile.
fn decode(slides: &Slides, key: StrokeKey, source: &SlideSource) -> anyhow::Result<Arc<Decoded>> {
    if let Some(decoded) = locked(slides).get(&key)
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
    locked(slides).insert(key, Arc::clone(&decoded));
    Ok(decoded)
}

#[derive(Debug, Default)]
pub(crate) struct SlidePlayer {
    /// Slides that were played; the others show their start.
    clocks: HashMap<StrokeKey, Clock>,
    /// Decoded on first use, also while drawing.
    decoded: Arc<Slides>,
    /// The slide the lecturer's screen shows against the setting, until another is stepped to.
    /// Slides to play once decoded off the UI thread, with where the outcome arrives.
    starting: HashMap<StrokeKey, mpsc::Receiver<Decoding>>,
    flipped: Option<StrokeKey>,
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
        decode(&self.decoded, key, source)
    }

    /// The decoded slide of a played slide; it was decoded when it started.
    fn played(&self, key: StrokeKey) -> Option<Arc<Decoded>> {
        locked(&self.decoded).get(&key).cloned()
    }

    /// Whether any slide was played. While none was, no slide is playing or held.
    pub(crate) fn any_played(&self) -> bool {
        !self.clocks.is_empty() || !self.starting.is_empty()
    }

    /// Whether the lecturer's screen animates `key` under `setting`.
    pub(crate) fn lecturer_animates(&self, key: StrokeKey, setting: LecturerSlides) -> bool {
        (setting == LecturerSlides::Animate) != (self.flipped == Some(key))
    }

    /// Show `key` against the setting on the lecturer's screen, or let it follow it again.
    pub(crate) fn flip(&mut self, key: StrokeKey) {
        self.flipped = (self.flipped != Some(key)).then_some(key);
    }

    /// Let every slide but `key` follow the setting again, e.g. after stepping to `key`.
    pub(crate) fn unflip_others(&mut self, key: Option<StrokeKey>) {
        if self.flipped != key {
            self.flipped = None;
        }
    }

    /// Whether any slide is playing now.
    pub(crate) fn playing(&self) -> bool {
        !self.starting.is_empty()
            || self
                .clocks
                .values()
                .any(|clock| matches!(clock, Clock::Playing { .. }))
    }

    pub(crate) fn position(&self, key: StrokeKey) -> Position {
        // Still decoding: it plays as soon as it can.
        if self.starting.contains_key(&key) {
            return Position::Playing;
        }
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

    /// Play `source` from its start. One not decoded yet is decoded on another thread and
    /// starts on the first frame after, see [Self::frame_at].
    pub(crate) fn play(&mut self, key: StrokeKey, source: &SlideSource) -> anyhow::Result<()> {
        let decoded = self
            .played(key)
            .is_some_and(|decoded| source.is_same_take(&decoded.take));
        if decoded {
            return self.play_at(key, source, Instant::now());
        }
        if self.starting.contains_key(&key) {
            return Ok(());
        }

        let (outcome_tx, outcome_rx) = mpsc::channel();
        let (slides, source) = (Arc::clone(&self.decoded), source.clone());
        std::thread::spawn(move || {
            let outcome = match decode(&slides, key, &source) {
                Ok(_) => Decoding::Done,
                Err(e) => {
                    error!("Decoding Manim slide failed, Err: {e:?}");
                    Decoding::Failed
                }
            };
            let _ = outcome_tx.send(outcome);
        });
        self.starting.insert(key, outcome_rx);
        Ok(())
    }

    /// Wait for a slide still decoding to start, e.g. when stepped again meanwhile.
    fn settle(&mut self, key: StrokeKey) {
        let Some(outcome_rx) = self.starting.remove(&key) else {
            return;
        };
        if outcome_rx.recv() == Ok(Decoding::Done) {
            self.start(key, Instant::now());
        }
    }

    /// Start the clock of a decoded slide.
    fn start(&mut self, key: StrokeKey, now: Instant) {
        self.clocks.insert(
            key,
            Clock::Playing {
                since: now,
                from: 0.0,
            },
        );
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
        self.settle(key);
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
                    // A loop left playing would run off screen and keep the views redrawing.
                    self.clocks.insert(key, Clock::Paused(slide.duration));
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
        // One still decoding has just started: back rewinds it to its start, as any playing.
        if self.starting.contains_key(&key) {
            self.settle(key);
            let Some(clock) = self.clocks.get_mut(&key) else {
                return Step::Leave;
            };
            *clock = Clock::Paused(0.0);
            return Step::Stay;
        }

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
        self.settle(key);
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
        self.starting.remove(&key);
        self.clocks.remove(&key);
        locked(&self.decoded).remove(&key);
    }

    /// Forget the playback of every slide that `stroke_of` no longer gives the take it played:
    /// its stroke was undone, redone into another take, or the document was replaced.
    ///
    /// The store hands the keys of undone strokes out again, so a clock left behind would
    /// pass to the next slide imported.
    pub(crate) fn forget_stale<'a>(&mut self, stroke_of: impl Fn(StrokeKey) -> Option<&'a Stroke>) {
        let mut decoded = locked(&self.decoded);

        decoded.retain(|&key, decoded| {
            stroke_of(key)
                .and_then(slide_source)
                .is_some_and(|source| source.is_same_take(&decoded.take))
        });
        self.clocks.retain(|key, _| decoded.contains_key(key));
        self.starting
            .retain(|key, _| stroke_of(*key).and_then(slide_source).is_some());
        self.flipped = self
            .flipped
            .filter(|key| stroke_of(*key).and_then(slide_source).is_some());
    }

    /// Advance the clocks: slides halt at the end of the part they play.
    pub(crate) fn frame(&mut self) -> FrameResult {
        self.frame_at(Instant::now())
    }

    pub(crate) fn frame_at(&mut self, now: Instant) -> FrameResult {
        let mut result = FrameResult::default();

        // Slides decoded since the last frame start now, from their first frame.
        let mut decoded_now = Vec::new();
        self.starting
            .retain(|&key, outcome_rx| match outcome_rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => true,
                Ok(Decoding::Done) => {
                    decoded_now.push(key);
                    false
                }
                Ok(Decoding::Failed) | Err(mpsc::TryRecvError::Disconnected) => false,
            });
        for key in decoded_now {
            self.start(key, now);
        }
        result.playing = !self.starting.is_empty();

        let decoded = locked(&self.decoded);

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
    fn is_live(&self, key: StrokeKey, stroke: &Stroke) -> bool {
        // The lecturer's end state is the stored still frame, drawn like any image.
        let animates = match self.view {
            SlideView::Audience => true,
            SlideView::Lecturer(setting) => self.player.lecturer_animates(key, setting),
        };
        slide_source(stroke).is_some() && animates
    }

    fn still_frame(&self, key: StrokeKey, _stroke: &Stroke) -> Option<u64> {
        // A halted slide shows the frame of its time; one not played yet its start.
        match self.player.clocks.get(&key) {
            Some(Clock::Playing { .. }) => None,
            Some(Clock::Paused(t)) => Some(t.to_bits()),
            None => Some(0.0_f64.to_bits()),
        }
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

        decoded
            .slide
            .draw_at(cx, t, target(bounds), self.cues(source))
    }

    fn still_job(&self, key: StrokeKey, stroke: &Stroke) -> Option<DrawJob> {
        let source = slide_source(stroke)?.clone();
        let slides = Arc::clone(&self.player.decoded);
        let clock = self.player.clocks.get(&key).copied();
        let cues = self.cues(&source);

        // Decoding is left to the job: it would stall the thread that asks.
        Some(Box::new(move |cx, bounds| {
            let decoded = decode(&slides, key, &source)?;
            let t = clock.map_or(0.0, |clock| decoded.time_at(clock, Instant::now()));

            decoded.slide.draw_at(cx, t, target(bounds), cues)
        }))
    }
}

impl ViewSlides<'_> {
    /// Whether the view shows the cues of `source`: the audience never sees them.
    fn cues(&self, source: &SlideSource) -> Cues {
        match self.view {
            SlideView::Audience => Cues::Hidden,
            SlideView::Lecturer(_) => source.cues(),
        }
    }
}

fn target(bounds: Aabb) -> kurbo::Rect {
    kurbo::Rect::new(
        bounds.mins[0],
        bounds.mins[1],
        bounds.maxs[0],
        bounds.maxs[1],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Image;
    use crate::strokes::VectorImage;
    use p2d::math::Vector2;
    use std::path::Path;

    const TAKE: &str = "tests/data/manimslide/take/single.take";
    /// The take's pixel size.
    const TAKE_SIZE: Vector2 = Vector2::new(480.0, 270.0);

    #[test]
    fn still_job_converts_the_take_when_run() {
        let take = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(TAKE)).unwrap();
        let source = SlideSource::from_take(&take, "Single".to_owned(), String::new()).unwrap();
        let page = Aabb::new(Vector2::ZERO, TAKE_SIZE);
        let stroke = Stroke::VectorImage(VectorImage::from_manim_slide(source, page).unwrap());
        let (player, key) = (SlidePlayer::default(), StrokeKey::default());

        // Asking is done on the UI thread: it must not convert.
        let job = player
            .view(SlideView::Audience)
            .still_job(key, &stroke)
            .unwrap();
        assert!(player.played(key).is_none());

        // The player keeps what the job converted, to play it without converting again.
        Image::gen_with_piet(|cx| job(cx, page), page, 1.0).unwrap();
        assert!(player.played(key).is_some());
    }

    /// Converting a take takes up to a second: playing one not converted yet must not stall
    /// the UI thread that asks.
    #[test]
    fn play_converts_the_take_off_thread() {
        let take = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(TAKE)).unwrap();
        let source = SlideSource::from_take(&take, "Single".to_owned(), String::new()).unwrap();
        let (mut player, key) = (SlidePlayer::default(), StrokeKey::default());

        player.play(key, &source).unwrap();
        assert!(player.starting.contains_key(&key));
        assert_eq!(player.position(key), Position::Playing);

        // Stepped on meanwhile, it waits for the take, then finishes as if it had played.
        assert_eq!(player.forward(key), Step::Stay);
        assert_eq!(player.position(key), Position::End);
    }
}
