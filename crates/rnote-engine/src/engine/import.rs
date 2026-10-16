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
use rayon::prelude::*;
use rnote_compose::ext::Vector2Ext;
use rnote_compose::shapes::Shapeable;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::error;

/// A ManimGX scene file, run: its code and the take of each of its scenes.
#[derive(Debug, Clone)]
pub struct ManimScene {
    pub path: PathBuf,
    pub code: String,
    /// In the file's order; each becomes one slide.
    pub takes: Vec<SceneTake>,
}

/// One scene's take: ManimGX's recording of its run.
#[derive(Debug, Clone)]
pub struct SceneTake {
    /// The scene's class name.
    pub name: String,
    pub take: Vec<u8>,
    /// Hash of what made the take (the driver's `slide_hash`), to skip it when unchanged.
    pub hash: Option<String>,
}

impl ManimScene {
    /// A slide source per scene, remembering the file and a copy of its code.
    fn sources(&self) -> impl Iterator<Item = anyhow::Result<SlideSource>> + '_ {
        // Takes convert independently, so on all cores.
        let sources: Vec<_> = self
            .takes
            .par_iter()
            .map(|t| {
                let source = SlideSource::from_take(&t.take, t.name.clone(), self.code.clone())?;
                Ok(source
                    .with_file(Some(self.path.clone()))
                    .with_hash(t.hash.clone()))
            })
            .collect();
        sources.into_iter()
    }
}

/// Whether imported slides also get a still copy in a second page column, for notes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NotesColumn {
    #[default]
    Without,
    With,
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

/// Replacement slides for an update, made off the main thread.
#[derive(Debug)]
pub struct PreparedUpdate {
    replacements: Vec<(StrokeKey, VectorImage)>,
    update: SlideUpdate,
}

/// A slide in the document an update may replace.
struct UpdateTarget {
    key: StrokeKey,
    name: String,
    image: VectorImage,
}

/// Replacements for `targets` by `sources`, matched by scene name; unreadable sources and
/// those matching no slide are counted.
fn replacements(
    targets: Vec<UpdateTarget>,
    sources: Vec<anyhow::Result<SlideSource>>,
) -> PreparedUpdate {
    let mut prepared = PreparedUpdate {
        replacements: vec![],
        update: SlideUpdate::default(),
    };

    for source in sources {
        let source = match source {
            Ok(source) => source,
            Err(e) => {
                error!("Reading Manim slide for update failed, Err: {e:?}");
                prepared.update.unreadable += 1;
                continue;
            }
        };
        let name = source.name().to_owned();

        let matching: Vec<_> = targets.iter().filter(|t| t.name == name).collect();
        if matching.is_empty() {
            prepared.update.unmatched.push(name);
            continue;
        }

        // A slide with a notes copy leaves its cues to the copy.
        let beside_notes = matching.iter().any(|t| {
            t.image
                .manim_slide
                .as_ref()
                .is_some_and(|s| s.is_notes_copy())
        });

        for target in matching {
            // A source that knows its file keeps it; a bare one takes the slide's.
            let old_file = target.image.manim_slide.as_ref().and_then(|s| s.file());
            let file = source.file().or(old_file).map(Path::to_path_buf);
            let source = source.clone().with_file(file);
            let source = if beside_notes {
                source.beside_notes()
            } else {
                source
            };

            match target.image.with_manim_slide(source) {
                Ok(image) => prepared.replacements.push((target.key, image)),
                Err(e) => {
                    error!("Updating Manim slide `{name}` failed, Err: {e:?}");
                    prepared.update.unreadable += 1;
                }
            }
        }
    }
    prepared
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

    /// Generate one slide per scene of a run ManimGX scene file, one per document page.
    ///
    /// Slides fill the pages from the one in view on, in the file's order.
    #[allow(clippy::type_complexity)]
    pub fn generate_manim_scene_slides(
        &self,
        scene: ManimScene,
        notes: NotesColumn,
    ) -> oneshot::Receiver<anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>>> {
        let (oneshot_sender, oneshot_receiver) =
            oneshot::channel::<anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>>>();
        let pages = self.manim_slide_pages(scene.takes.len());

        rayon::spawn(move || {
            let result = scene
                .sources()
                .collect::<anyhow::Result<Vec<_>>>()
                .and_then(|sources| Self::manim_slide_strokes(sources, pages, notes));
            if oneshot_sender.send(result).is_err() {
                error!(
                    "Sending result to receiver while importing a Manim scene failed. Receiver already dropped"
                );
            }
        });

        oneshot_receiver
    }

    /// Slide strokes for `sources` on `pages`, on the document layer. With a notes column,
    /// each slide also gets a still copy on the page to its right.
    fn manim_slide_strokes(
        sources: Vec<SlideSource>,
        pages: Vec<Aabb>,
        notes: NotesColumn,
    ) -> anyhow::Result<Vec<(Stroke, Option<StrokeLayer>)>> {
        // Sequential on purpose: drawing still frames on several threads at once is many
        // times slower (measured, 30 slides: 0.7 s alone, 11 s on all cores).
        let mut strokes = Vec::with_capacity(sources.len() * 2);

        for (source, page) in sources.into_iter().zip(pages) {
            if notes == NotesColumn::With {
                let shift = Vector2::new(page.extents()[0], 0.0);
                let right = Aabb::new(page.mins + shift, page.maxs + shift);
                let copy = VectorImage::from_manim_slide(source.clone().into_notes_copy(), right)?;
                strokes.push((Stroke::VectorImage(copy), Some(StrokeLayer::Document)));
            }

            let source = match notes {
                NotesColumn::With => source.beside_notes(),
                NotesColumn::Without => source,
            };
            let image = VectorImage::from_manim_slide(source, page)?;
            strokes.push((Stroke::VectorImage(image), Some(StrokeLayer::Document)));
        }
        Ok(strokes)
    }

    /// Replacements for the document's slides of the scenes in `scene` (a re-run scene file),
    /// by scene name, made off the main thread; `apply_manim_update` puts them in.
    pub fn prepare_manim_update(&self, scene: ManimScene) -> oneshot::Receiver<PreparedUpdate> {
        let (oneshot_sender, oneshot_receiver) = oneshot::channel::<PreparedUpdate>();
        let targets = self.update_targets();

        rayon::spawn(move || {
            let prepared = replacements(targets, scene.sources().collect());
            if oneshot_sender.send(prepared).is_err() {
                error!(
                    "Sending result to receiver while updating Manim slides failed. Receiver already dropped"
                );
            }
        });

        oneshot_receiver
    }

    /// Replace slides with their prepared replacements, in place: ink on top stays, and
    /// the replacement is one undo step.
    pub fn apply_manim_update(&mut self, prepared: PreparedUpdate) -> (WidgetFlags, SlideUpdate) {
        let mut widget_flags = WidgetFlags::default();
        let PreparedUpdate {
            replacements,
            mut update,
        } = prepared;

        for (key, image) in replacements {
            // The slide may have been deleted while its replacement was made.
            let Some(stroke) = self.store.get_stroke_mut(key) else {
                continue;
            };
            *stroke = Stroke::VectorImage(image);
            self.store.update_geometry_for_stroke(key);
            self.slides.forget(key);
            update.updated += 1;
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

    /// `prepare_manim_update` and `apply_manim_update` in one, on this thread.
    #[cfg(test)]
    fn update_manim_scene(&mut self, scene: ManimScene) -> (WidgetFlags, SlideUpdate) {
        let prepared = replacements(self.update_targets(), scene.sources().collect());
        self.apply_manim_update(prepared)
    }

    /// The scene files the document's ManimGX slides came from, to run them again.
    pub fn manim_scene_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self
            .store
            .stroke_keys_as_rendered()
            .into_iter()
            .filter_map(|key| match self.store.get_stroke_ref(key) {
                Some(Stroke::VectorImage(image)) => image
                    .manim_slide
                    .as_ref()
                    .as_ref()?
                    .file()
                    .map(Path::to_path_buf),
                _ => None,
            })
            .collect();
        files.sort();
        files.dedup();
        files
    }

    /// The hashes of the slides made from scene file `file`, by scene name: an update need
    /// not run those again whose hash is unchanged.
    pub fn manim_slide_hashes(&self, file: &Path) -> HashMap<String, String> {
        self.store
            .stroke_keys_unordered()
            .into_iter()
            .filter_map(|key| match self.store.get_stroke_ref(key) {
                Some(Stroke::VectorImage(image)) => {
                    let source = image.manim_slide.as_ref()?;
                    if source.file() != Some(file) {
                        return None;
                    }
                    Some((source.name().to_owned(), source.hash()?.to_owned()))
                }
                _ => None,
            })
            .collect()
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

    /// The document's slides, for an update to replace.
    fn update_targets(&self) -> Vec<UpdateTarget> {
        self.store
            .stroke_keys_unordered()
            .into_iter()
            .filter_map(|key| {
                let Some(Stroke::VectorImage(image)) = self.store.get_stroke_ref(key) else {
                    return None;
                };
                let name = image.manim_slide.as_ref()?.name().to_owned();
                Some(UpdateTarget {
                    key,
                    name,
                    image: image.clone(),
                })
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
    use crate::manimslide::SlideRole;

    const TAKE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/manimslide/take/");

    fn scene_take(name: &str, file: &str) -> SceneTake {
        SceneTake {
            name: name.to_owned(),
            take: std::fs::read(format!("{TAKE_DIR}{file}")).unwrap(),
            hash: Some(format!("hash of {name}")),
        }
    }

    /// `lecture01.py` with scenes Spike and Parts.
    fn lecture_scene(code: &str) -> ManimScene {
        ManimScene {
            path: PathBuf::from("lecture01.py"),
            code: code.to_owned(),
            takes: vec![
                scene_take("Spike", "spike.take"),
                scene_take("Parts", "parts.take"),
            ],
        }
    }

    /// `cues.py` with one scene, Cue: a square and a cue circle.
    fn cue_scene() -> ManimScene {
        ManimScene {
            path: PathBuf::from("cues.py"),
            code: String::new(),
            takes: vec![scene_take("Cue", "cue.take")],
        }
    }

    fn cue_sources() -> Vec<SlideSource> {
        cue_scene()
            .sources()
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap()
    }

    /// Paths in the still frame of the slide or notes copy among `strokes`.
    fn still_paths(strokes: &[(Stroke, Option<StrokeLayer>)], role: SlideRole) -> usize {
        strokes
            .iter()
            .find_map(|(stroke, _)| match stroke {
                Stroke::VectorImage(image)
                    if image
                        .manim_slide
                        .as_ref()
                        .is_some_and(|s| (s.is_notes_copy()) == (role == SlideRole::NotesCopy)) =>
                {
                    Some(image.svg_data.matches("<path").count())
                }
                _ => None,
            })
            .unwrap()
    }

    fn rows(engine: &Engine, count: usize) -> Vec<Aabb> {
        let page = engine.document.config.format.size();
        (0..count)
            .map(|row| {
                let top = Vector2::new(0.0, page[1] * row as f64);
                Aabb::new(top, top + page)
            })
            .collect()
    }

    /// The code a slide keeps; notes copies keep none.
    fn code_of(engine: &Engine, key: StrokeKey) -> Option<String> {
        match engine.store.get_stroke_ref(key) {
            Some(Stroke::VectorImage(image)) => image
                .manim_slide
                .as_ref()
                .unwrap()
                .scene_code()
                .map(str::to_owned),
            _ => panic!("not a slide"),
        }
    }

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

    #[test]
    fn notes_column_copies_each_slide_to_the_right() {
        let engine = Engine::default();
        let page = engine.document.config.format.size();
        let sources = lecture_scene("")
            .sources()
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap();

        let strokes =
            Engine::manim_slide_strokes(sources.clone(), rows(&engine, 2), NotesColumn::With)
                .unwrap();
        assert_eq!(strokes.len(), 4);
        let notes: Vec<_> = strokes
            .iter()
            .filter_map(|(stroke, _)| match stroke {
                Stroke::VectorImage(image) => Some(image),
                _ => None,
            })
            .filter(|image| {
                image
                    .manim_slide
                    .as_ref()
                    .is_some_and(|s| s.is_notes_copy())
            })
            .collect();
        assert_eq!(notes.len(), 2);
        assert!(
            notes
                .iter()
                .all(|image| image.bounds().mins[0] >= page[0] - 1e-6)
        );

        let without =
            Engine::manim_slide_strokes(sources, rows(&engine, 2), NotesColumn::Without).unwrap();
        assert_eq!(without.len(), 2);
    }

    #[test]
    fn cues_show_in_the_notes_copy_instead_of_the_slide() {
        let engine = Engine::default();
        let with = Engine::manim_slide_strokes(cue_sources(), rows(&engine, 1), NotesColumn::With)
            .unwrap();
        let without =
            Engine::manim_slide_strokes(cue_sources(), rows(&engine, 1), NotesColumn::Without)
                .unwrap();

        let copy = still_paths(&with, SlideRole::NotesCopy);
        assert!(still_paths(&with, SlideRole::Live) < copy);
        // Without a copy, the slide is the only place its cues show.
        assert_eq!(still_paths(&without, SlideRole::Live), copy);
    }

    #[test]
    fn update_moves_cues_to_an_existing_notes_copy() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));

        // A slide imported with its cues, beside a notes copy (as before cues moved).
        let with = Engine::manim_slide_strokes(cue_sources(), rows(&engine, 1), NotesColumn::With)
            .unwrap();
        let without =
            Engine::manim_slide_strokes(cue_sources(), rows(&engine, 1), NotesColumn::Without)
                .unwrap();
        let copy = with
            .into_iter()
            .find(|(stroke, _)| match stroke {
                Stroke::VectorImage(image) => image
                    .manim_slide
                    .as_ref()
                    .is_some_and(|s| s.is_notes_copy()),
                _ => false,
            })
            .unwrap();
        let mut keys = vec![];
        for (stroke, layer) in without.into_iter().chain([copy]) {
            keys.push(engine.store.insert_stroke(stroke, layer));
        }
        let _ = engine.record(Instant::now());

        let (_, update) = engine.update_manim_scene(cue_scene());
        assert_eq!(update.updated, 2);
        let paths = |key| match engine.store.get_stroke_ref(key) {
            Some(Stroke::VectorImage(image)) => image.svg_data.matches("<path").count(),
            _ => panic!("not a slide"),
        };
        assert!(paths(keys[0]) < paths(keys[1]));
    }

    #[test]
    fn slides_keep_the_hash_of_what_made_them() {
        let mut engine = Engine::default();
        for (stroke, layer) in Engine::manim_slide_strokes(
            lecture_scene("")
                .sources()
                .collect::<anyhow::Result<Vec<_>>>()
                .unwrap(),
            rows(&engine, 2),
            NotesColumn::With,
        )
        .unwrap()
        {
            engine.store.insert_stroke(stroke, layer);
        }

        let hashes = engine.manim_slide_hashes(Path::new("lecture01.py"));
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes["Spike"], "hash of Spike");
        assert!(engine.manim_slide_hashes(Path::new("other.py")).is_empty());
    }

    #[test]
    fn scene_takes_are_stored_with_their_code() {
        let engine = Engine::default();
        let strokes = Engine::manim_slide_strokes(
            lecture_scene("class Spike(m.Scene): ...")
                .sources()
                .collect::<anyhow::Result<Vec<_>>>()
                .unwrap(),
            rows(&engine, 2),
            NotesColumn::Without,
        )
        .unwrap();

        let Stroke::VectorImage(image) = &strokes[0].0 else {
            panic!("not a slide");
        };
        assert!(image.svg_data.contains("<path"), "still frame has no paths");

        // Saved and loaded again, the take, the scene and its code are kept.
        let json = serde_json::to_string(image).unwrap();
        let loaded: VectorImage = serde_json::from_str(&json).unwrap();
        let source = loaded.manim_slide.unwrap();
        assert_eq!(source.name(), "Spike");
        assert_eq!(source.scene_code(), Some("class Spike(m.Scene): ..."));
        assert_eq!(source.file(), Some(Path::new("lecture01.py")));
        assert!(source.still_frame().is_ok());
    }

    #[test]
    fn notes_copies_store_only_their_still_frame() {
        let engine = Engine::default();
        let strokes = Engine::manim_slide_strokes(
            lecture_scene("")
                .sources()
                .collect::<anyhow::Result<Vec<_>>>()
                .unwrap(),
            rows(&engine, 1),
            NotesColumn::With,
        )
        .unwrap();
        let saved: Vec<serde_json::Value> = strokes
            .iter()
            .map(|(stroke, _)| match stroke {
                Stroke::VectorImage(image) => serde_json::to_value(image).unwrap(),
                _ => panic!("not a slide"),
            })
            .collect();

        let (copies, slides): (Vec<_>, Vec<_>) = saved
            .iter()
            .partition(|image| image["manim_slide"]["role"] == "notes_copy");
        assert_eq!((copies.len(), slides.len()), (1, 1));
        assert!(slides[0]["manim_slide"].get("take").is_some());
        assert!(copies[0]["manim_slide"].get("take").is_none());

        // Loaded again, the copy still knows its scene, for updates.
        let copy: VectorImage = serde_json::from_value(copies[0].clone()).unwrap();
        let source = copy.manim_slide.unwrap();
        assert!(source.is_notes_copy());
        assert_eq!(source.name(), "Spike");
    }

    #[test]
    fn slides_of_the_earlier_slide_file_format_open_as_still_frames() {
        let image = VectorImage::default();
        let mut json: serde_json::Value = serde_json::to_value(&image).unwrap();
        json["manim_slide"] = serde_json::json!({"slide_json": "{}", "time": 1.0});

        let loaded: VectorImage = serde_json::from_value(json).unwrap();
        assert!(loaded.manim_slide.is_none());
    }

    #[test]
    fn scene_update_replaces_slides_by_scene_name() {
        let mut engine = Engine::default();
        let _ = engine.camera_set_size(Vector2::new(800.0, 600.0));
        let mut keys = vec![];
        for (stroke, layer) in Engine::manim_slide_strokes(
            lecture_scene("# first")
                .sources()
                .collect::<anyhow::Result<Vec<_>>>()
                .unwrap(),
            rows(&engine, 2),
            NotesColumn::With,
        )
        .unwrap()
        {
            keys.push(engine.store.insert_stroke(stroke, layer));
        }
        let _ = engine.record(Instant::now());
        assert_eq!(
            engine.manim_scene_files(),
            vec![PathBuf::from("lecture01.py")]
        );

        // Only Spike runs again: it and its notes copy are replaced, Parts stays.
        let mut changed = lecture_scene("# second");
        changed.takes.truncate(1);
        let (_, update) = engine.update_manim_scene(changed);
        assert_eq!(update.updated, 2);
        assert!(update.unmatched.is_empty());
        let codes: Vec<String> = keys
            .iter()
            .filter_map(|&key| code_of(&engine, key))
            .collect();
        assert_eq!(codes, vec!["# second".to_owned(), "# first".to_owned()]);

        // One undo step brings the first version back.
        let _ = engine.undo(Instant::now());
        let codes: Vec<String> = keys
            .iter()
            .filter_map(|&key| code_of(&engine, key))
            .collect();
        assert_eq!(codes, vec!["# first".to_owned(), "# first".to_owned()]);
    }
}
