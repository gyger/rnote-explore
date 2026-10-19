// Imports
use super::{StrokeKey, StrokeStore};
use crate::strokes::Stroke;
use crate::{Camera, Drawable, Image};
use p2d::bounding_volume::{Aabb, BoundingVolume};
use p2d::math::Vector2;
use rnote_compose::shapes::Shapeable;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Stroke rasters for the views without per-stroke renderings (projector, zoom panel).
///
/// Re-rasterizing every stroke on every frame grows with the ink and starves the other
/// windows. The store edits strokes copy-on-write, so a shared stroke never changes in place:
/// the `Arc` held here stays the exact content it was rasterized from. A unique stroke is
/// edited in place, e.g. by the pen drawing it, and is drawn immediate instead.
///
/// ```text
///  shared stroke (count > 1) ──► cached raster, reused while Arc::ptr_eq
///  unique stroke (count = 1) ──► drawn immediate each frame
/// ```
///
/// A stroke turns shared once the history holds a copy of the stroke map and the map is
/// copied for the next edit, i.e. latest when the next stroke starts.
#[derive(Debug, Default)]
pub(crate) struct RasterCache {
    entries: RefCell<HashMap<StrokeKey, Entry>>,
}

#[derive(Debug)]
struct Entry {
    stroke: Arc<Stroke>,
    image_scale: f64,
    /// Rasterized part of the stroke, in document coordinates.
    region: Aabb,
    node: gtk4::gsk::RenderNode,
}

impl Entry {
    fn fits(&self, stroke: &Arc<Stroke>, image_scale: f64, visible: Aabb) -> bool {
        Arc::ptr_eq(&self.stroke, stroke)
            && self.image_scale == image_scale
            && self.region.contains(&visible)
    }
}

impl RasterCache {
    /// Extra raster around the viewport, as a share of its size, so small pans reuse it.
    const MARGIN: f64 = 0.5;

    /// Cached raster of `stroke`, rasterized on a miss. `None` for a stroke still being edited
    /// in place, or outside the viewport.
    fn node(
        &self,
        key: StrokeKey,
        stroke: &Arc<Stroke>,
        viewport: Aabb,
        image_scale: f64,
    ) -> anyhow::Result<Option<gtk4::gsk::RenderNode>> {
        // Unique: not yet recorded, the pens mutate it without a copy.
        if Arc::strong_count(stroke) == 1 {
            return Ok(None);
        }
        let Some(visible) = stroke.bounds().intersection(&viewport) else {
            return Ok(None);
        };

        let mut entries = self.entries.borrow_mut();
        if let Some(entry) = entries.get(&key)
            && entry.fits(stroke, image_scale, visible)
        {
            return Ok(Some(entry.node.clone()));
        }

        // Large strokes at high zoom: raster only around the viewport.
        let around = viewport.loosened(viewport.half_extents().max_element() * Self::MARGIN);
        let Some(region) = stroke.bounds().intersection(&around) else {
            return Ok(None);
        };
        let node = Image::gen_with_piet(|cx| stroke.draw(cx, image_scale), region, image_scale)?
            .to_rendernode()?;

        entries.insert(
            key,
            Entry {
                stroke: Arc::clone(stroke),
                image_scale,
                region,
                node: node.clone(),
            },
        );
        Ok(Some(node))
    }

    /// Drop rasters of strokes no longer shown.
    fn retain(&self, shown: &HashSet<StrokeKey>) {
        self.entries.borrow_mut().retain(|key, _| shown.contains(key));
    }
}

impl StrokeStore {
    /// Draw `paper` and the strokes in `viewport` for a view without per-stroke renderings.
    /// Finished strokes come from `cache`, the rest is drawn immediate, clipped to `clip`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_strokes_cached(
        &self,
        snapshot: &gtk4::Snapshot,
        camera: &Camera,
        viewport: Aabb,
        clip: Aabb,
        cache: &RasterCache,
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
        let draw_keys =
            |keys: &[StrokeKey], shown: &mut HashSet<StrokeKey>| -> anyhow::Result<()> {
                let mut run = Vec::new();
                for &key in keys {
                    let Some(stroke) = self.stroke_components.get(key) else {
                        continue;
                    };
                    // Live strokes change every frame, so they are never cached.
                    if live.is_live(key, stroke) {
                        run.push(key);
                        continue;
                    }
                    let Some(node) = cache.node(key, stroke, viewport, image_scale)? else {
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

    struct NoLive;

    impl crate::store::LiveStrokes for NoLive {
        fn is_live(&self, _key: StrokeKey, _stroke: &Stroke) -> bool {
            false
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
                &NoLive,
                |_| {},
            )
            .unwrap();

        let node = snapshot.to_node().expect("strokes drawn");
        assert!(has_multiply(&node));
    }
}
