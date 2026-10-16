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
mod render;
mod timeline;

#[cfg(test)]
mod tests;

use render::SlideImages;
use serde::{Deserialize, Serialize};
use timeline::Slide;

/// A slide kept in a document: the slide file's text and the time it shows.
///
/// The file text is kept verbatim, so a document stays readable by any
/// player of the slide format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "manim_slide")]
pub(crate) struct SlideSource {
    #[serde(rename = "slide_json")]
    slide_json: String,
    #[serde(rename = "time")]
    time: f64,
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
            slide_json,
        })
    }

    /// The slide at its shown time, drawn once.
    pub(crate) fn still_frame(&self) -> anyhow::Result<StillFrame> {
        let slide = Slide::from_json(&self.slide_json)?;
        let images = SlideImages::decode(&slide)?;

        Ok(StillFrame {
            svg: slide.still_svg(&images, self.time)?,
            width: slide.frame.pw,
            height: slide.frame.ph,
        })
    }
}
