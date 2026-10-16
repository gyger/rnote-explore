//! Manim vector slides: reading and evaluating `.slide.json` files.
//!
//! The format is specified in the Manim fork,
//! `manim/renderer/vector/slide-format.md`; `SlideTimeline.evaluate` there is
//! the reference this port is tested against.
//!
//! ```text
//!  .slide.json ──format──► Slide ──evaluate(t)──► states ──draw──► DrawNode list ──render──► piet
//!   (packed)                (tracks, states,       (blend, arc,       (paths and paint,
//!                            eases, draw order)     partial ops)       Manim units, y up)
//! ```
//!
//! The rest of the engine only sees [SlideSource]: a slide as stored in a document.

// Parts of the model are only read by tests until playback uses them.
#![allow(dead_code)]

mod draw;
mod format;
mod ops;
mod player;
mod render;
mod timeline;

#[cfg(test)]
mod tests;

pub(crate) use player::{Position, SlidePlayer, SlideView};
use render::SlideImages;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use timeline::Slide;

/// A slide kept in a document: the slide file's text and the time it shows.
///
/// The file text is kept verbatim, so a document stays readable by any
/// player of the slide format. It is shared, so copies of the stroke (undo
/// history, clipboard) cost nothing and the player can tell them apart from
/// an updated slide.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "manim_slide")]
pub(crate) struct SlideSource {
    #[serde(rename = "slide_json")]
    slide_json: Arc<str>,
    #[serde(rename = "time")]
    time: f64,
    /// Where the slide was imported from, to find its deck again for updates.
    #[serde(rename = "file", default, skip_serializing_if = "Option::is_none")]
    file: Option<PathBuf>,
}

/// A still frame of a slide: an SVG document of the slide's pixel size.
#[derive(Debug, Clone)]
pub(crate) struct StillFrame {
    pub(crate) svg: String,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl SlideSource {
    /// Read a slide file; the slide shows its end state.
    pub(crate) fn from_json(slide_json: String) -> anyhow::Result<Self> {
        let slide = Slide::from_json(&slide_json)?;
        Ok(Self {
            time: slide.duration,
            slide_json: slide_json.into(),
            file: None,
        })
    }

    /// Remember the file the slide was read from.
    pub(crate) fn with_file(mut self, file: Option<PathBuf>) -> Self {
        self.file = file;
        self
    }

    pub(crate) fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// The slide's section name, which identifies it across renders of its deck.
    pub(crate) fn name(&self) -> anyhow::Result<String> {
        Ok(Slide::from_json(&self.slide_json)?.name)
    }

    /// Whether `other` is this very slide text, not just equal to it.
    fn is_same_text(&self, other: &Arc<str>) -> bool {
        Arc::ptr_eq(&self.slide_json, other)
    }

    #[cfg(test)]
    pub(crate) fn slide_json(&self) -> String {
        self.slide_json.to_string()
    }

    /// The slide and its images, ready to draw at any time.
    fn decode(&self) -> anyhow::Result<(Slide, SlideImages)> {
        let slide = Slide::from_json(&self.slide_json)?;
        let images = SlideImages::decode(&slide)?;
        Ok((slide, images))
    }

    /// The slide at its shown time, drawn once.
    pub(crate) fn still_frame(&self) -> anyhow::Result<StillFrame> {
        let (slide, images) = self.decode()?;

        Ok(StillFrame {
            svg: slide.still_svg(&images, self.time)?,
            width: slide.frame.pw,
            height: slide.frame.ph,
        })
    }
}
