// Imports
use super::{DrawJob, StrokeKey, StrokeStore};
use crate::engine::{EngineTask, EngineTaskSender};
use crate::strokes::Stroke;
use crate::{Camera, Drawable, Image};
use p2d::bounding_volume::{Aabb, BoundingVolume};
use p2d::math::Vector2;
use rnote_compose::penevent::PenProgress;
use rnote_compose::shapes::Shapeable;
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use tracing::error;

/// Stroke rasters for the views without per-stroke renderings (projector, zoom panel).
///
/// Re-rasterizing every stroke on every frame grows with the ink and starves the other
/// windows. The store edits strokes copy-on-write, so a shared stroke never changes in place:
/// the `Arc` held here stays the exact content it was rasterized from. A unique stroke is
/// edited in place while a pen works, e.g. the stroke being drawn: holding it would copy it
/// on every pen event, so it is drawn immediate instead.
///
/// ```text
///  shared stroke (count > 1) ──────────────► cached raster, reused while Arc::ptr_eq
///  unique stroke (count = 1), pen at rest ─► cached raster; its next edit copies it once
///  unique stroke (count = 1), pen working ─► drawn immediate each frame
/// ```
///
/// A stroke turns shared once the history holds a copy of the stroke map and the map is
/// copied for the next edit. A document just opened has no shared stroke at all.
///
/// A live stroke standing still is cached like any other, as the still frame it shows.
///
/// A cache [in the background](Self::in_background) never rasterizes while drawing: the UI
/// thread draws every window, so a page of slides rasterized there stalls them all.
///
/// ```text
///  UI thread                     worker thread            UI thread
///  request_rasters ── order ───► rasterize ── task ─────► accept ─► cached raster
///  draw: the stroke immediate, or the view stays on what it showed (see `Rasters`)
/// ```
#[derive(Debug, Default)]
pub(crate) struct RasterCache {
    misses: Misses,
    entries: RefCell<HashMap<StrokeKey, Entry>>,
    /// Rasters ordered from the worker and not here yet.
    orders: RefCell<HashMap<StrokeKey, Order>>,
    /// Strokes asked for since the last draw: their rasters are kept though not drawn yet.
    awaited: RefCell<HashSet<StrokeKey>>,
    worker: Worker,
}

/// Where a cache rasterizes a stroke it has no raster of.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Misses {
    /// While drawing: the view waits for the raster.
    #[default]
    Inline,
    /// On a worker thread, once asked for with [StrokeStore::request_rasters].
    Background,
}

/// Whether the rasters a view asked for are there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rasters {
    Ready,
    /// Some are being made: drawing now would draw their strokes immediate.
    Awaited,
}

/// What a raster shows.
#[derive(Debug)]
struct Subject {
    stroke: Arc<Stroke>,
    image_scale: f64,
    /// The still frame of a live stroke the raster shows; `None` for the stroke itself.
    still: Option<u64>,
    /// Rasterized part of the stroke, in document coordinates.
    region: Aabb,
}

impl Subject {
    fn fits(
        &self,
        stroke: &Arc<Stroke>,
        image_scale: f64,
        still: Option<u64>,
        visible: Aabb,
    ) -> bool {
        Arc::ptr_eq(&self.stroke, stroke)
            && self.image_scale == image_scale
            && self.still == still
            && self.region.contains(&visible)
    }
}

#[derive(Debug)]
struct Entry {
    subject: Subject,
    node: gtk4::gsk::RenderNode,
}

/// A raster ordered from the worker.
#[derive(Debug)]
struct Order {
    subject: Subject,
    progress: Progress,
    /// Set once nobody waits for the raster; also tells this order from a later one.
    called_off: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Progress {
    Working,
    /// Not ordered again: the stroke is drawn immediate, where the failure shows.
    Failed,
}

/// A raster back from the worker, on its way to its cache as an [EngineTask].
#[derive(Debug, Clone)]
pub struct Rasterized {
    key: StrokeKey,
    /// The `called_off` of the order it answers.
    order: Arc<AtomicBool>,
    /// `None` if rasterizing failed.
    image: Option<Image>,
}

type Job = Box<dyn FnOnce() + Send>;

/// The one thread that rasterizes in the background, in the order asked. One, not a pool:
/// the other cores stay with the main canvas, and slides keep their pictures per thread.
#[derive(Debug, Default)]
struct Worker {
    jobs: OnceCell<mpsc::Sender<Job>>,
}

impl Worker {
    /// Queue `job`; the thread starts with the first one and ends with the cache.
    fn run(&self, job: Job) {
        let jobs = self.jobs.get_or_init(|| {
            let (jobs, queue) = mpsc::channel::<Job>();
            std::thread::spawn(move || queue.into_iter().for_each(|job| job()));
            jobs
        });

        // The thread only ends with the cache: jobs catch their panics.
        let _ = jobs.send(job);
    }
}

impl RasterCache {
    /// Extra raster around the viewport, as a share of its size, so small pans reuse it.
    const MARGIN: f64 = 0.5;

    /// A cache that rasterizes on a worker thread, for a view that must not stall the others.
    pub(crate) fn in_background() -> Self {
        Self {
            misses: Misses::Background,
            ..Self::default()
        }
    }

    /// The part of `stroke` in the viewport. `None` for a stroke `pen` may be editing in
    /// place, or outside the viewport: no raster is held of those.
    fn visible(stroke: &Arc<Stroke>, viewport: Aabb, pen: PenProgress) -> Option<Aabb> {
        // Unique: not yet recorded, a working pen mutates it without a copy.
        if Arc::strong_count(stroke) == 1 && pen == PenProgress::InProgress {
            return None;
        }
        stroke.bounds().intersection(&viewport)
    }

    /// The part of `stroke` to rasterize. Large strokes at high zoom: only around the viewport.
    fn region(stroke: &Arc<Stroke>, viewport: Aabb) -> Option<Aabb> {
        let around = viewport.loosened(viewport.half_extents().max_element() * Self::MARGIN);
        stroke.bounds().intersection(&around)
    }

    /// Cached raster of `stroke`; on a miss rasterized inline, or `None` in the background.
    /// `None` also for a stroke `pen` may be editing in place, or outside the viewport.
    ///
    /// `still` is the frame a live stroke stands on, with who draws it.
    fn node(
        &self,
        key: StrokeKey,
        stroke: &Arc<Stroke>,
        viewport: Aabb,
        image_scale: f64,
        pen: PenProgress,
        still: Option<(u64, &dyn super::LiveStrokes)>,
    ) -> anyhow::Result<Option<gtk4::gsk::RenderNode>> {
        let frame = still.map(|(frame, _)| frame);
        let Some(visible) = Self::visible(stroke, viewport, pen) else {
            return Ok(None);
        };

        let mut entries = self.entries.borrow_mut();
        if let Some(entry) = entries.get(&key)
            && entry.subject.fits(stroke, image_scale, frame, visible)
        {
            return Ok(Some(entry.node.clone()));
        }
        if self.misses == Misses::Background {
            return Ok(None);
        }

        let Some(region) = Self::region(stroke, viewport) else {
            return Ok(None);
        };
        let draw = |cx: &mut piet_cairo::CairoRenderContext| match still {
            Some((_, live)) => live.draw_live(key, stroke, cx, stroke.bounds()),
            None => stroke.draw(cx, image_scale),
        };
        let node = Image::gen_with_piet(draw, region, image_scale)?.to_rendernode()?;

        entries.insert(
            key,
            Entry {
                subject: Subject {
                    stroke: Arc::clone(stroke),
                    image_scale,
                    still: frame,
                    region,
                },
                node: node.clone(),
            },
        );
        Ok(Some(node))
    }

    /// Order the raster of `stroke` from the worker, unless it is here or under way. It
    /// comes back as an [EngineTask] through `tasks_tx`, to be handed to [Self::accept].
    #[allow(clippy::too_many_arguments)]
    fn request(
        &self,
        key: StrokeKey,
        stroke: &Arc<Stroke>,
        viewport: Aabb,
        image_scale: f64,
        pen: PenProgress,
        still: Option<(u64, &dyn super::LiveStrokes)>,
        tasks_tx: &EngineTaskSender,
    ) {
        let frame = still.map(|(frame, _)| frame);
        let Some(visible) = Self::visible(stroke, viewport, pen) else {
            return;
        };
        self.awaited.borrow_mut().insert(key);

        let fits = |subject: &Subject| subject.fits(stroke, image_scale, frame, visible);
        if self
            .entries
            .borrow()
            .get(&key)
            .is_some_and(|entry| fits(&entry.subject))
        {
            return;
        }
        let mut orders = self.orders.borrow_mut();
        if orders.get(&key).is_some_and(|order| fits(&order.subject)) {
            return;
        }

        let Some(region) = Self::region(stroke, viewport) else {
            return;
        };
        // A still frame is drawn by who plays it, any other stroke draws itself.
        let draw: DrawJob = match still {
            Some((_, live)) => {
                let Some(draw) = live.still_job(key, stroke) else {
                    return;
                };
                draw
            }
            None => {
                let stroke = Arc::clone(stroke);
                Box::new(move |cx, _bounds| stroke.draw(cx, image_scale))
            }
        };

        let called_off = Arc::new(AtomicBool::new(false));
        let order = Order {
            subject: Subject {
                stroke: Arc::clone(stroke),
                image_scale,
                still: frame,
                region,
            },
            progress: Progress::Working,
            called_off: Arc::clone(&called_off),
        };
        // An order for what the stroke showed before is of no use any more.
        if let Some(outdated) = orders.insert(key, order) {
            outdated.called_off.store(true, Ordering::Relaxed);
        }
        drop(orders);

        let bounds = stroke.bounds();
        let tasks_tx = tasks_tx.clone();
        self.worker.run(Box::new(move || {
            if called_off.load(Ordering::Relaxed) {
                return;
            }

            // Always answer, also after a panic: a view may wait for this raster.
            let image = catch_unwind(AssertUnwindSafe(|| {
                Image::gen_with_piet(|cx| draw(cx, bounds), region, image_scale)
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("rasterizing panicked")))
            .inspect_err(|e| error!("Rasterizing a stroke in the background failed, Err: {e:?}"))
            .ok();

            tasks_tx.send(EngineTask::AudienceRaster(Rasterized {
                key,
                order: called_off,
                image,
            }));
        }));
    }

    /// Whether the raster of the stroke is being made.
    fn awaits(&self, key: StrokeKey) -> bool {
        self.orders
            .borrow()
            .get(&key)
            .is_some_and(|order| order.progress == Progress::Working)
    }

    /// Take in a raster from the worker. One nobody waits for any more is dropped.
    pub(crate) fn accept(&self, rasterized: Rasterized) {
        let mut orders = self.orders.borrow_mut();
        let Some(order) = orders.get_mut(&rasterized.key) else {
            return;
        };
        if !Arc::ptr_eq(&order.called_off, &rasterized.order) {
            return;
        }

        let node = rasterized.image.and_then(|image| {
            image
                .to_rendernode()
                .inspect_err(|e| error!("Creating the rendernode of a raster failed, Err: {e:?}"))
                .ok()
        });
        let Some(node) = node else {
            order.progress = Progress::Failed;
            return;
        };

        if let Some(order) = orders.remove(&rasterized.key) {
            let subject = order.subject;
            self.entries
                .borrow_mut()
                .insert(rasterized.key, Entry { subject, node });
        }
    }

    /// Drop rasters of strokes neither shown nor asked for, and call off their orders.
    fn retain(&self, shown: &HashSet<StrokeKey>) {
        let awaited = self.awaited.take();
        let kept = |key: &StrokeKey| shown.contains(key) || awaited.contains(key);

        self.entries.borrow_mut().retain(|key, _| kept(key));
        self.orders.borrow_mut().retain(|key, order| {
            if !kept(key) {
                order.called_off.store(true, Ordering::Relaxed);
            }
            kept(key)
        });
    }
}

/// What a raster of a stroke shows.
enum Pose {
    /// The stroke itself.
    Plain,
    /// The still frame a live stroke stands on.
    Still(u64),
    /// Nothing: a live stroke that moves is another picture every frame.
    Moving,
}

impl Pose {
    fn of(live: &dyn super::LiveStrokes, key: StrokeKey, stroke: &Stroke) -> Self {
        if !live.is_live(key, stroke) {
            return Self::Plain;
        }
        match live.still_frame(key, stroke) {
            Some(frame) => Self::Still(frame),
            None => Self::Moving,
        }
    }
}

impl StrokeStore {
    /// Order the rasters of the strokes in `viewport` that `cache` lacks; they arrive through
    /// `tasks_tx`. Call before each draw from a cache in the background: a draw drops the
    /// rasters of strokes neither drawn nor asked for since the draw before.
    pub(crate) fn request_rasters(
        &self,
        camera: &Camera,
        viewport: Aabb,
        cache: &RasterCache,
        pen: PenProgress,
        live: &dyn super::LiveStrokes,
        tasks_tx: &EngineTaskSender,
    ) -> Rasters {
        let image_scale = camera.image_scale();
        let (ink, markers) = self.stroke_keys_as_rendered_intersecting_bounds_partitioned(viewport);
        let mut rasters = Rasters::Ready;

        for key in ink.into_iter().chain(markers) {
            let Some(stroke) = self.stroke_components.get(key) else {
                continue;
            };
            let still = match Pose::of(live, key, stroke) {
                Pose::Plain => None,
                Pose::Still(frame) => Some((frame, live)),
                Pose::Moving => continue,
            };

            cache.request(key, stroke, viewport, image_scale, pen, still, tasks_tx);
            if cache.awaits(key) {
                rasters = Rasters::Awaited;
            }
        }
        rasters
    }

    /// Draw `paper` and the strokes in `viewport` for a view without per-stroke renderings.
    /// Strokes come from `cache`; those `pen` may be editing and live ones that move are
    /// drawn immediate, clipped to `clip`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_strokes_cached(
        &self,
        snapshot: &gtk4::Snapshot,
        camera: &Camera,
        viewport: Aabb,
        clip: Aabb,
        cache: &RasterCache,
        pen: PenProgress,
        live: &dyn super::LiveStrokes,
        paper: impl FnOnce(&gtk4::Snapshot),
    ) -> anyhow::Result<()> {
        use crate::ext::GrapheneRectExt;
        use gtk4::{graphene, prelude::*};
        use piet::RenderContext;
        use rnote_compose::ext::{AabbExt, DAffine2Ext};

        let image_scale = camera.image_scale();
        let surface_bounds = Aabb::new(Vector2::ZERO, camera.size());

        // Consecutive uncached strokes share one cairo node, keeping the stacking order.
        let draw_run = |keys: &mut Vec<StrokeKey>| -> anyhow::Result<()> {
            if keys.is_empty() {
                return Ok(());
            }
            let cairo_cx = snapshot.append_cairo(&graphene::Rect::from_p2d_aabb(surface_bounds));
            let mut piet_cx = piet_cairo::CairoRenderContext::new(&cairo_cx);
            piet_cx.transform(camera.transform().to_kurbo());
            piet_cx.clip(clip.to_kurbo_rect());
            for &key in keys.iter() {
                match self.stroke_components.get(key) {
                    Some(stroke) if live.is_live(key, stroke) => {
                        live.draw_live(key, stroke, &mut piet_cx, stroke.bounds())?
                    }
                    _ => self.draw_keys_immediate(&[key], &mut piet_cx, image_scale)?,
                }
            }
            piet_cx.finish().map_err(|e| anyhow::anyhow!("{e:?}"))?;
            keys.clear();
            Ok(())
        };

        // Strokes in stacking order; uncached ones are batched into runs.
        let draw_keys = |keys: &[StrokeKey],
                         shown: &mut HashSet<StrokeKey>|
         -> anyhow::Result<()> {
            let mut run = Vec::new();
            for &key in keys {
                let Some(stroke) = self.stroke_components.get(key) else {
                    continue;
                };
                // A live stroke is cached only while it stands still.
                let still = match Pose::of(live, key, stroke) {
                    Pose::Plain => None,
                    Pose::Still(frame) => Some((frame, live)),
                    Pose::Moving => {
                        run.push(key);
                        continue;
                    }
                };
                let Some(node) = cache.node(key, stroke, viewport, image_scale, pen, still)? else {
                    run.push(key);
                    continue;
                };
                draw_run(&mut run)?;
                shown.insert(key);

                snapshot.save();
                snapshot.transform(Some(&camera.transform_for_gtk_snapshot()));
                snapshot.push_clip(&graphene::Rect::from_p2d_aabb(clip));
                snapshot.append_node(&node);
                snapshot.pop();
                snapshot.restore();
            }
            draw_run(&mut run)
        };

        // Markers multiply with the paper and everything below, as on the main canvas:
        //   Blend(bottom: Blend(bottom: paper + ink, top: marker 1), top: marker 2)
        let (ink, markers) = self.stroke_keys_as_rendered_intersecting_bounds_partitioned(viewport);
        for _ in markers.iter() {
            snapshot.push_blend(gtk4::gsk::BlendMode::Multiply);
        }

        let mut shown = HashSet::new();
        paper(snapshot);
        draw_keys(&ink, &mut shown)?;

        for &key in markers.iter() {
            // End the bottom of this marker's blend, then draw the marker as its top.
            snapshot.pop();
            draw_keys(&[key], &mut shown)?;
            snapshot.pop();
        }

        cache.retain(&shown);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::chrono_comp::StrokeLayer;
    use crate::strokes::BrushStroke;
    use gtk4::gsk;
    use gtk4::prelude::*;
    use rnote_compose::Style;
    use rnote_compose::Transformable;
    use rnote_compose::penpath::Element;

    /// The cache relies on it: a held stroke is copied on edit, never changed in place.
    #[test]
    fn held_stroke_is_copied_on_edit() {
        let mut store = StrokeStore::default();
        let start = Element::new(Vector2::new(10.0, 10.0), 1.0);
        let key = store.insert_stroke(
            Stroke::BrushStroke(BrushStroke::new(start, Style::default())),
            None,
        );
        let held = Arc::clone(store.stroke_components.get(key).unwrap());
        let bounds = held.bounds();

        store
            .get_stroke_mut(key)
            .unwrap()
            .translate(Vector2::new(5.0, 0.0));

        let current = store.stroke_components.get(key).unwrap();
        assert!(!Arc::ptr_eq(&held, current));
        assert_eq!(held.bounds(), bounds);
    }

    fn store_with_stroke() -> (StrokeStore, StrokeKey) {
        let mut store = StrokeStore::default();
        let start = Element::new(Vector2::new(10.0, 10.0), 1.0);
        let key = store.insert_stroke(
            Stroke::BrushStroke(BrushStroke::new(start, Style::default())),
            None,
        );
        (store, key)
    }

    /// A deck just opened has no shared stroke yet; it must not be rasterized every frame.
    #[test]
    fn unrecorded_stroke_is_cached_unless_a_pen_edits() {
        let (store, key) = store_with_stroke();
        let stroke = store.stroke_components.get(key).unwrap();
        let viewport = Aabb::new(Vector2::ZERO, Vector2::new(100.0, 100.0));
        let cache = RasterCache::default();

        let editing = cache.node(key, stroke, viewport, 1.0, PenProgress::InProgress, None);
        assert!(editing.unwrap().is_none());

        let idle = cache.node(key, stroke, viewport, 1.0, PenProgress::Idle, None);
        assert!(idle.unwrap().is_some());
    }

    /// A live stroke standing still; counts how often it is drawn.
    #[derive(Default)]
    struct StillLive {
        frame: std::cell::Cell<u64>,
        drawn: std::cell::Cell<usize>,
        /// The threads its still jobs ran on.
        jobs_ran_on: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>,
    }

    impl crate::store::LiveStrokes for StillLive {
        fn is_live(&self, _key: StrokeKey, _stroke: &Stroke) -> bool {
            true
        }

        fn still_frame(&self, _key: StrokeKey, _stroke: &Stroke) -> Option<u64> {
            Some(self.frame.get())
        }

        fn draw_live(
            &self,
            _key: StrokeKey,
            _stroke: &Stroke,
            _cx: &mut piet_cairo::CairoRenderContext,
            _bounds: Aabb,
        ) -> anyhow::Result<()> {
            self.drawn.set(self.drawn.get() + 1);
            Ok(())
        }

        fn still_job(&self, _key: StrokeKey, _stroke: &Stroke) -> Option<DrawJob> {
            let ran_on = Arc::clone(&self.jobs_ran_on);
            Some(Box::new(move |_cx, _bounds| {
                ran_on.lock().unwrap().push(std::thread::current().id());
                Ok(())
            }))
        }
    }

    /// The next raster the worker sends back.
    fn next_raster(engine: &mut crate::Engine) -> Rasterized {
        let mut tasks_rx = engine.take_engine_tasks_rx().unwrap();
        match futures::executor::block_on(tasks_rx.recv()) {
            Some(EngineTask::AudienceRaster(rasterized)) => rasterized,
            other => panic!("expected a raster, got {other:?}"),
        }
    }

    /// The UI thread draws every window: a raster made on it stalls the main canvas too.
    #[test]
    fn background_cache_rasterizes_off_the_asking_thread() {
        let (store, key) = store_with_stroke();
        let stroke = store.stroke_components.get(key).unwrap();
        let viewport = Aabb::new(Vector2::ZERO, Vector2::new(100.0, 100.0));
        let (cache, live) = (RasterCache::in_background(), StillLive::default());
        let mut engine = crate::Engine::default();
        let tasks_tx = engine.engine_tasks_tx();
        let request = || {
            store.request_rasters(
                &Camera::default(),
                viewport,
                &cache,
                PenProgress::Idle,
                &live,
                &tasks_tx,
            )
        };
        let node = || {
            let still = Some((0, &live as &dyn crate::store::LiveStrokes));
            cache
                .node(key, stroke, viewport, 1.0, PenProgress::Idle, still)
                .unwrap()
        };

        // Ordered, not drawn: nothing to show yet.
        assert_eq!(request(), Rasters::Awaited);
        assert!(node().is_none());
        assert_eq!(live.drawn.get(), 0);

        cache.accept(next_raster(&mut engine));
        let ran_on = live.jobs_ran_on.lock().unwrap().clone();
        assert_eq!(ran_on.len(), 1);
        assert_ne!(ran_on[0], std::thread::current().id());

        // Asking again orders nothing more.
        assert_eq!(request(), Rasters::Ready);
        assert!(node().is_some());
        assert_eq!(live.jobs_ran_on.lock().unwrap().len(), 1);
    }

    /// Scrolling on leaves rasters behind that nobody will look at.
    #[test]
    fn raster_of_a_stroke_scrolled_away_is_dropped() {
        let (store, key) = store_with_stroke();
        let stroke = store.stroke_components.get(key).unwrap();
        let viewport = Aabb::new(Vector2::ZERO, Vector2::new(100.0, 100.0));
        let cache = RasterCache::in_background();
        let mut engine = crate::Engine::default();
        let tasks_tx = engine.engine_tasks_tx();

        cache.request(
            key,
            stroke,
            viewport,
            1.0,
            PenProgress::Idle,
            None,
            &tasks_tx,
        );
        let rasterized = next_raster(&mut engine);

        // A draw keeps what was asked for before it; the next draw without asking does not.
        let nothing_shown = HashSet::new();
        cache.retain(&nothing_shown);
        assert!(cache.awaits(key));
        cache.retain(&nothing_shown);
        assert!(!cache.awaits(key));

        cache.accept(rasterized);
        let node = cache.node(key, stroke, viewport, 1.0, PenProgress::Idle, None);
        assert!(node.unwrap().is_none());
    }

    #[test]
    fn still_live_stroke_is_drawn_once_per_frame_it_shows() {
        let (store, key) = store_with_stroke();
        let stroke = store.stroke_components.get(key).unwrap();
        let viewport = Aabb::new(Vector2::ZERO, Vector2::new(100.0, 100.0));
        let (cache, live) = (RasterCache::default(), StillLive::default());
        let node = |frame| {
            cache
                .node(
                    key,
                    stroke,
                    viewport,
                    1.0,
                    PenProgress::Idle,
                    Some((frame, &live)),
                )
                .unwrap()
        };

        // Redraws of the same frame reuse its raster.
        assert!(node(0).is_some());
        assert!(node(0).is_some());
        assert_eq!(live.drawn.get(), 1);

        // Another frame is another picture.
        assert!(node(1).is_some());
        assert_eq!(live.drawn.get(), 2);
    }

    struct NoLive;

    impl crate::store::LiveStrokes for NoLive {
        fn is_live(&self, _key: StrokeKey, _stroke: &Stroke) -> bool {
            false
        }

        fn still_frame(&self, _key: StrokeKey, _stroke: &Stroke) -> Option<u64> {
            None
        }

        fn draw_live(
            &self,
            _key: StrokeKey,
            _stroke: &Stroke,
            _cx: &mut piet_cairo::CairoRenderContext,
            _bounds: Aabb,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        fn still_job(&self, _key: StrokeKey, _stroke: &Stroke) -> Option<DrawJob> {
            None
        }
    }

    /// Whether the node tree holds a multiply blend, e.g. `Clip > Blend(Multiply) > ..`.
    fn has_multiply(node: &gsk::RenderNode) -> bool {
        if let Some(blend) = node.downcast_ref::<gsk::BlendNode>() {
            if blend.blend_mode() == gsk::BlendMode::Multiply {
                return true;
            }
            return has_multiply(&blend.bottom_child()) || has_multiply(&blend.top_child());
        }
        if let Some(container) = node.downcast_ref::<gsk::ContainerNode>() {
            return (0..container.n_children()).any(|i| has_multiply(&container.child(i)));
        }
        if let Some(clip) = node.downcast_ref::<gsk::ClipNode>() {
            return has_multiply(&clip.child());
        }
        if let Some(transform) = node.downcast_ref::<gsk::TransformNode>() {
            return has_multiply(&transform.child());
        }
        false
    }

    /// Markers multiply with the paper and ink below, as on the main canvas.
    #[test]
    fn marker_blends_multiply() {
        // Snapshots need GTK; headless runs have no display to initialize it on.
        if gtk4::init().is_err() {
            return;
        }

        let mut store = StrokeStore::default();
        let start = Element::new(Vector2::new(10.0, 10.0), 1.0);
        let stroke = Stroke::BrushStroke(BrushStroke::new(start, Style::default()));
        store.insert_stroke(stroke.clone(), None);
        store.insert_stroke(stroke, Some(StrokeLayer::Highlighter));

        let camera = Camera::default();
        let viewport = camera.viewport();
        let snapshot = gtk4::Snapshot::new();
        store
            .draw_strokes_cached(
                &snapshot,
                &camera,
                viewport,
                viewport,
                &RasterCache::default(),
                PenProgress::Idle,
                &NoLive,
                |_| {},
            )
            .unwrap();

        let node = snapshot.to_node().expect("strokes drawn");
        assert!(has_multiply(&node));
    }
}
