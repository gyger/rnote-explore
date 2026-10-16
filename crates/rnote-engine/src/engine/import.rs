// Imports
use super::StrokeContent;
use crate::document::Layout;
use crate::engine_view_mut;
use crate::manimslide::SlideSource;
use crate::pens::Pen;
use crate::pens::PenStyle;
use crate::store::StrokeKey;
use crate::store::chrono_comp::StrokeLayer;
use crate::strokes::{BitmapImage, Stroke, VectorImage};
use crate::strokes::{Resize, resize::ImageSizeOption, resize::calculate_resize_ratio};
use crate::{Engine, WidgetFlags};
use futures::channel::oneshot;
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_compose::ext::Vector2Ext;
use rnote_compose::shapes::Shapeable;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::error;

/// A Manim slide file to import: its text and where it was read from.
#[derive(Debug, Clone)]
pub struct ManimSlideFile {
    pub json: String,
    pub path: Option<PathBuf>,
}

/// What updating slides from re-rendered files did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlideUpdate {
    /// Slides in the document that were replaced.
    pub updated: usize,
    /// Names of given slides that are not in the document.
    pub unmatched: Vec<String>,
    /// Given files that could not be read, or slides that could not be redrawn.
    pub unreadable: usize,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, num_derive::FromPrimitive, num_derive::ToPrimitive,
)]
#[serde(rename = "pdf_import_pages_type")]
pub enum PdfImportPagesType {
    #[serde(rename = "bitmap")]
    Bitmap = 0,
    #[serde(rename = "vector")]
    Vector,
}

impl Default for PdfImportPagesType {
    fn default() -> Self {
        Self::Vector
    }
}

impl TryFrom<u32> for PdfImportPagesType {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        num_traits::FromPrimitive::from_u32(value).ok_or_else(|| {
            anyhow::anyhow!(
                "PdfImportPagesType try_from::<u32>() for value {} failed",
                value
            )
        })
    }
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, num_derive::FromPrimitive, num_derive::ToPrimitive,
)]
#[serde(rename = "pdf_import_page_spacing")]
pub enum PdfImportPageSpacing {
    #[serde(rename = "continuous")]
    Continuous = 0,
    #[serde(rename = "one_per_document_page")]
    OnePerDocumentPage,
}

impl Default for PdfImportPageSpacing {
    fn default() -> Self {
        Self::Continuous
    }
}

impl TryFrom<u32> for PdfImportPageSpacing {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        num_traits::FromPrimitive::from_u32(value).ok_or_else(|| {
            anyhow::anyhow!(
                "PdfImportPageSpacing try_from::<u32>() for value {} failed",
                value
            )
        })
    }
}

/// Pdf import preferences.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default, rename = "pdf_import_prefs")]
pub struct PdfImportPrefs {
    /// Pdf page width in percentage to the format width.
    #[serde(rename = "page_width_perc")]
    pub page_width_perc: f64,
    /// Pdf page spacing.
    #[serde(rename = "page_spacing")]
    pub page_spacing: PdfImportPageSpacing,
    /// Pdf pages import type.
    #[serde(rename = "pages_type")]
    pub pages_type: PdfImportPagesType,
    /// The scalefactor when importing as bitmap image
    #[serde(rename = "bitmap_scalefactor")]
    pub bitmap_scalefactor: f64,
    /// Whether the document layout should be adjusted to the Pdf
    #[serde(rename = "adjust_document")]
    pub adjust_document: bool,
}

impl Default for PdfImportPrefs {
    fn default() -> Self {
        Self {
            pages_type: PdfImportPagesType::default(),
            page_width_perc: 50.0,
            page_spacing: PdfImportPageSpacing::default(),
            bitmap_scalefactor: 1.8,
            adjust_document: false,
        }
    }
}

/// Xournal++ `.xopp` file import preferences.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename = "xopp_import_prefs")]
pub struct XoppImportPrefs {
    /// Import DPI.
    #[serde(rename = "pages_type")]
    pub dpi: f64,
}

impl Default for XoppImportPrefs {
    fn default() -> Self {
        Self { dpi: 96.0 }
    }
}

/// Import preferences.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default, rename = "import_prefs")]
pub struct ImportPrefs {
    /// Pdf import preferences
    #[serde(rename = "pdf_import_prefs")]
    pub pdf_import_prefs: PdfImportPrefs,
    /// Xournal++ `.xopp` file import preferences
    #[serde(rename = "xopp_import_prefs")]
    pub xopp_import_prefs: XoppImportPrefs,
}

impl Engine {
    /// Generate a vectorimage from the bytes.
    ///
    /// The bytes are expected to be from a valid UTF-8 encoded Svg string.
    pub fn generate_vectorimage_from_bytes(
        &self,
        pos: Vector2,
        bytes: Vec<u8>,
        respect_borders: bool,
    ) -> oneshot::Receiver<anyhow::Result<VectorImage>> {
        let (oneshot_sender, oneshot_receiver) = oneshot::channel::<anyhow::Result<VectorImage>>();

        let resize_struct = Resize {
            width: self.document.config.format.width(),
            height: self.document.config.format.height(),
            layout_fixed_width: self.document.config.layout.is_fixed_width(),
            max_viewpoint: Some(self.camera.viewport().maxs),
            restrain_to_viewport: true,
            respect_borders,
        };
        rayon::spawn(move || {
            let result = || -> anyhow::Result<VectorImage> {
                let svg_str = String::from_utf8(bytes)?;

                VectorImage::from_svg_str(
                    &svg_str,
                    pos,
                    ImageSizeOption::ResizeImage(resize_struct),
                )
            };

            if oneshot_sender.send(result()).is_err() {
                error!(
                    "Sending result to receiver while generating VectorImage from bytes failed. Receiver already dropped."
                );
            }
        });

        oneshot_receiver
    }

    /// Generate a bitmapimage for the bytes.
    ///
    /// The bytes are expected to be from a valid bitmap image (Png/Jpeg).
    pub fn generate_bitmapimage_from_bytes(
        &self,
        pos: Vector2,
        bytes: Vec<u8>,
        respect_borders: bool,
    ) -> oneshot::Receiver<anyhow::Result<BitmapImage>> {
        let (oneshot_sender, oneshot_receiver) = oneshot::channel::<anyhow::Result<BitmapImage>>();

        let resize_struct = Resize {
            width: self.document.config.format.width(),
            height: self.document.config.format.height(),
            layout_fixed_width: self.document.config.layout.is_fixed_width(),
            max_viewpoint: Some(self.camera.viewport().maxs),
            restrain_to_viewport: true,
            respect_borders,
        };
        rayon::spawn(move || {
            let result = || -> anyhow::Result<BitmapImage> {
                BitmapImage::from_image_bytes(
                    &bytes,
                    pos,
                    ImageSizeOption::ResizeImage(resize_struct),
                )
            };

            if oneshot_sender.send(result()).is_err() {
                error!(
                    "Sending result to receiver while generating BitmapImage from bytes failed. Receiver already dropped."
                );
            }
        });

        oneshot_receiver
    }

    /// Generate image strokes for each page for the bytes.
    ///
    /// The bytes are expected to be from a valid Pdf.
    ///
    /// Note: `insert_pos` does not have an effect when the `adjust_document` import pref is set true.
    #[allow(clippy::type_complexity)]
    pub fn generate_pdf_pages_from_bytes(
        &self,
        bytes: Vec<u8>,
        insert_pos: Vector2,
        page_range: Option<Range<usize>>,
        password: Option<String>,
    ) -> oneshot::Receiver<anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>>> {
        let (oneshot_sender, oneshot_receiver) =
            oneshot::channel::<anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>>>();
        let pdf_import_prefs = self.config.read().import_prefs.pdf_import_prefs;
        let format = self.document.config.format;
        let insert_pos = if self
            .config
            .read()
            .import_prefs
            .pdf_import_prefs
            .adjust_document
        {
            Vector2::ZERO
        } else {
            insert_pos
        };

        rayon::spawn(move || {
            let result = || -> anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>> {
                match pdf_import_prefs.pages_type {
                    PdfImportPagesType::Bitmap => {
                        let bitmapimages = BitmapImage::from_pdf_bytes(
                            &bytes,
                            pdf_import_prefs,
                            insert_pos,
                            page_range,
                            &format,
                            password,
                        )?
                        .into_iter()
                        .map(|s| (Stroke::BitmapImage(s), Some(StrokeLayer::Document)))
                        .collect::<Vec<(Stroke, Option<StrokeLayer>)>>();
                        Ok(bitmapimages)
                    }
                    PdfImportPagesType::Vector => {
                        let vectorimages = VectorImage::from_pdf_bytes(
                            &bytes,
                            pdf_import_prefs,
                            insert_pos,
                            page_range,
                            &format,
                            password,
                        )?
                        .into_iter()
                        .map(|s| (Stroke::VectorImage(s), Some(StrokeLayer::Document)))
                        .collect::<Vec<(Stroke, Option<StrokeLayer>)>>();
                        Ok(vectorimages)
                    }
                }
            };

            if oneshot_sender.send(result()).is_err() {
                error!(
                    "Sending result to receiver while importing Pdf bytes failed. Receiver already dropped"
                );
            }
        });

        oneshot_receiver
    }

    /// `count` pages from the one under the center of the view on, downwards.
    ///
    /// Pages tile from the coordinate origin, like the format borders.
    fn manim_slide_pages(&self, count: usize) -> Vec<Aabb> {
        let size = self.document.config.format.size();
        let first = (self.camera.viewport_center()[1] / size[1]).floor();

        (0..count)
            .map(|i| {
                let top = Vector2::new(0.0, (first + i as f64) * size[1]);
                Aabb::new(top, top + size)
            })
            .collect()
    }

    /// Generate one image stroke per Manim slide file, one per document page.
    ///
    /// Slides fill the pages from the one in view on, in the given order.
    #[allow(clippy::type_complexity)]
    pub fn generate_manim_slides(
        &self,
        files: Vec<ManimSlideFile>,
    ) -> oneshot::Receiver<anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>>> {
        let (oneshot_sender, oneshot_receiver) =
            oneshot::channel::<anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>>>();
        let pages = self.manim_slide_pages(files.len());

        rayon::spawn(move || {
            let result = || -> anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>> {
                files
                    .into_iter()
                    .zip(pages)
                    .map(|(file, page)| {
                        let source = SlideSource::from_json(file.json)?.with_file(file.path);
                        let image = VectorImage::from_manim_slide(source, page)?;
                        Ok((Stroke::VectorImage(image), Some(StrokeLayer::Document)))
                    })
                    .collect()
            };

            if oneshot_sender.send(result()).is_err() {
                error!(
                    "Sending result to receiver while importing Manim slides failed. Receiver already dropped"
                );
            }
        });

        oneshot_receiver
    }

    /// Replace the document's slides with re-rendered ones of the same name, in place.
    ///
    /// Ink on top stays, and the replacement is one undo step. A slide name is its Manim
    /// section name; file numbers shift when sections are added, so they are not used.
    pub fn update_manim_slides(&mut self, slide_jsons: Vec<String>) -> (WidgetFlags, SlideUpdate) {
        let mut widget_flags = WidgetFlags::default();
        let mut update = SlideUpdate::default();
        let in_document = self.manim_slides_by_name();

        for slide_json in slide_jsons {
            let (source, name) = match SlideSource::from_json(slide_json)
                .and_then(|source| Ok((source.name()?, source)))
            {
                Ok((name, source)) => (source, name),
                Err(e) => {
                    error!("Reading Manim slide for update failed, Err: {e:?}");
                    update.unreadable += 1;
                    continue;
                }
            };

            let targets: Vec<_> = in_document.iter().filter(|(_, n, _)| *n == name).collect();
            if targets.is_empty() {
                update.unmatched.push(name);
                continue;
            }

            for (key, _, file) in targets {
                let source = source.clone().with_file(file.clone());
                let Some(Stroke::VectorImage(old)) = self.store.get_stroke_ref(*key) else {
                    continue;
                };
                match old.with_manim_slide(source) {
                    Ok(image) => {
                        if let Some(stroke) = self.store.get_stroke_mut(*key) {
                            *stroke = Stroke::VectorImage(image);
                        }
                        self.store.update_geometry_for_stroke(*key);
                        self.slides.forget(*key);
                        update.updated += 1;
                    }
                    Err(e) => {
                        error!("Updating Manim slide `{name}` failed, Err: {e:?}");
                        update.unreadable += 1;
                    }
                }
            }
        }

        if update.updated == 0 {
            return (widget_flags, update);
        }
        widget_flags |= self.store.record(Instant::now());
        widget_flags |= self.update_content_rendering_current_viewport();
        widget_flags.redraw = true;
        widget_flags.store_modified = true;
        (widget_flags, update)
    }

    /// The folder the document's slides were imported from, to pick updated files there.
    pub fn manim_slides_folder(&self) -> Option<PathBuf> {
        self.store
            .stroke_keys_as_rendered()
            .into_iter()
            .find_map(|key| match self.store.get_stroke_ref(key) {
                Some(Stroke::VectorImage(image)) => image
                    .manim_slide
                    .as_ref()?
                    .file()?
                    .parent()
                    .map(Path::to_path_buf),
                _ => None,
            })
    }

    /// The document's slides: key, name and source file.
    fn manim_slides_by_name(&self) -> Vec<(StrokeKey, String, Option<PathBuf>)> {
        self.store
            .stroke_keys_unordered()
            .into_iter()
            .filter_map(|key| {
                let Some(Stroke::VectorImage(image)) = self.store.get_stroke_ref(key) else {
                    return None;
                };
                let source = image.manim_slide.as_ref()?;
                let name = source.name().ok()?;
                Some((key, name, source.file().map(Path::to_path_buf)))
            })
            .collect()
    }

    /// Import the generated strokes into the store.
    pub fn import_generated_content(
        &mut self,
        strokes: Vec<(Stroke, Option<StrokeLayer>)>,
        adjust_document: bool,
    ) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();
        if strokes.is_empty() {
            return widget_flags;
        }
        let select = !adjust_document;

        // we need to always deselect all strokes. Even tough changing the pen style deselects too, it does only when
        // the pen is actually different.
        let all_strokes = self.store.stroke_keys_as_rendered();
        self.store.set_selected_keys(&all_strokes, false);

        if select {
            widget_flags |= self.change_pen_style(PenStyle::Selector);
        }

        if adjust_document {
            let max_size = strokes
                .iter()
                .map(|(stroke, _)| stroke.bounds().extents())
                .fold(Vector2::ZERO, |acc, x| acc.maxs(&x));
            self.document.config.format.set_width(max_size[0]);
            self.document.config.format.set_height(max_size[1]);
            widget_flags |= self.set_doc_layout(Layout::FixedSize) | self.doc_resize_autoexpand()
        }

        let inserted = strokes
            .into_iter()
            .map(|(stroke, layer)| self.store.insert_stroke(stroke, layer))
            .collect::<Vec<StrokeKey>>();

        // resize after the strokes are inserted, but before they are set selected
        widget_flags |= self.doc_resize_to_fit_content();
        if select {
            self.store.set_selected_keys(&inserted, true);
        }
        widget_flags |= self.current_pen_update_state();
        widget_flags |= self.store.record(Instant::now());
        widget_flags.resize = true;
        widget_flags.store_modified = true;
        widget_flags.refresh_ui = true;

        widget_flags
    }

    /// Insert text.
    pub fn insert_text(&mut self, text: String, pos: Option<Vector2>) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        // we need to always deselect all strokes. Even tough changing the pen style deselects too, but only when the pen is actually changed.
        let all_strokes = self.store.stroke_keys_as_rendered();
        self.store.set_selected_keys(&all_strokes, false);

        widget_flags |= self.change_pen_style(PenStyle::Typewriter);

        if let Pen::Typewriter(typewriter) = self.penholder.current_pen_mut() {
            widget_flags |= typewriter.insert_text(text, pos, &mut engine_view_mut!(self));
        }

        widget_flags |= self.store.record(Instant::now());
        widget_flags.redraw = true;
        widget_flags
    }

    /// Insert the stroke content.
    ///
    /// The data usually comes from the clipboard, drag-and-drop, ..
    pub fn insert_stroke_content(
        &mut self,
        content: StrokeContent,
        pos: Vector2,
        resize: ImageSizeOption,
    ) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        // we need to always deselect all strokes
        // even though changing the pen style deselects too, but only when the pen is actually different.
        let all_strokes = self.store.stroke_keys_as_rendered();
        self.store.set_selected_keys(&all_strokes, false);
        widget_flags |= self.change_pen_style(PenStyle::Selector);

        // calculate ratio
        let ratio = match resize {
            ImageSizeOption::ResizeImage(resize) => {
                calculate_resize_ratio(resize, content.size().unwrap(), pos)
            }
            _ => 1.0f64,
        };
        let inserted_keys = self.store.insert_stroke_content(content, ratio, pos);

        // re generate view
        self.store.update_geometry_for_strokes(&inserted_keys);
        self.store.regenerate_rendering_in_viewport_threaded(
            self.tasks_tx.clone(),
            false,
            self.camera.viewport(),
            self.camera.image_scale(),
        );

        widget_flags |= self
            .penholder
            .current_pen_update_state(&mut engine_view_mut!(self));

        widget_flags |= self.store.record(Instant::now());
        widget_flags.redraw = true;

        widget_flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manim_slides_start_at_the_page_in_view() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page = engine.document.config.format.size();

        // The view's top shows page 0, its center page 1.
        let _ = engine
            .camera
            .set_viewport_center(Vector2::new(page[0] * 0.5, page[1] * 1.1));

        let pages = engine.manim_slide_pages(2);
        assert_eq!(pages[0].mins, Vector2::new(0.0, page[1]));
        assert_eq!(pages[1].mins, Vector2::new(0.0, page[1] * 2.0));
    }

    fn slide_file(name: &str) -> String {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/manimslide/");
        std::fs::read_to_string(format!("{dir}{name}.slide.json")).unwrap()
    }

    fn slide_json(engine: &Engine, key: StrokeKey) -> String {
        match engine.store.get_stroke_ref(key) {
            Some(Stroke::VectorImage(image)) => image.manim_slide.as_ref().unwrap().slide_json(),
            _ => panic!("not a slide"),
        }
    }

    #[test]
    fn updating_slides_replaces_them_by_name_in_place() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let page = engine.document.config.format.size();

        let keys = ["0000_arcs", "0001_paint"].map(|name| {
            let source = SlideSource::from_json(slide_file(name)).unwrap();
            let image =
                VectorImage::from_manim_slide(source, Aabb::new(Vector2::ZERO, page)).unwrap();
            engine
                .store
                .insert_stroke(Stroke::VectorImage(image), Some(StrokeLayer::Document))
        });
        let _ = engine.record(Instant::now());
        let bounds_before = engine.store.get_stroke_ref(keys[0]).unwrap().bounds();

        // "0000_arcs-edited" is slide "arcs" with other stroke colors.
        let edited = slide_file("0000_arcs-edited");
        let (_, update) =
            engine.update_manim_slides(vec![edited.clone(), slide_file("0002_image")]);
        assert_eq!(update.updated, 1);
        assert_eq!(update.unmatched, vec!["image".to_owned()]);

        assert_eq!(slide_json(&engine, keys[0]), edited);
        assert_eq!(
            engine.store.get_stroke_ref(keys[0]).unwrap().bounds(),
            bounds_before
        );
        assert_eq!(slide_json(&engine, keys[1]), slide_file("0001_paint"));

        let _ = engine.undo(Instant::now());
        assert_eq!(slide_json(&engine, keys[0]), slide_file("0000_arcs"));
    }
}
