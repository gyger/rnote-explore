//! ManimGX scenes as slides: their takes, played in a document.
//!
//! ```text
//!  take (ManimGX) ──take::slide_from_take──► Slide ──evaluate(t)──► states ──draw──► piet
//!                                            (tracks, states,        (blend between
//!                                             draw order, stops)      recorded frames)
//! ```
//!
//! The rest of the engine only sees [SlideSource]: a slide as stored in a document.

mod draw;
mod ops;
mod player;
mod render;
mod take;
mod timeline;

#[cfg(test)]
mod tests;

use base64::Engine as _;
#[cfg(feature = "ui")]
pub(crate) use player::SlideView;
pub(crate) use player::{Position, SlidePlayer, Step};
use render::Cues;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use timeline::Slide;

/// A slide kept in a document: a ManimGX take as it came, and the time it shows.
///
/// The take is stored verbatim (base64), so a better converter improves old documents.
/// A notes copy is never played: it keeps only its still frame (in its image) and its
/// scene name, so updates still find it.
/// It is shared, so copies of the stroke (undo history, clipboard) cost nothing and the
/// player can tell them apart from an updated slide.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "manim_slide")]
pub(crate) struct SlideSource {
    #[serde(rename = "take", default, skip_serializing_if = "Option::is_none")]
    take: Option<Arc<str>>,
    #[serde(rename = "scene")]
    scene: Scene,
    #[serde(rename = "time")]
    time: f64,
    /// The scene file, to run it again on update.
    #[serde(rename = "file", default, skip_serializing_if = "Option::is_none")]
    file: Option<PathBuf>,
    #[serde(rename = "role", default, skip_serializing_if = "SlideRole::is_live")]
    role: SlideRole,
    /// Hash of what made the take, to skip re-running an unchanged slide on update.
    #[serde(rename = "hash", default, skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    /// The take as converted on import, until its still frames are drawn: converting is
    /// the slow part, so it is done once.
    #[serde(skip)]
    converted: Option<Arc<Slide>>,
}

/// The ManimGX scene a take records.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "manim_scene")]
struct Scene {
    /// The scene's class name, which identifies the slide across renders.
    #[serde(rename = "name")]
    name: String,
    /// A copy of the scene file's code: a record of what made the slide.
    #[serde(rename = "code", default, skip_serializing_if = "Option::is_none")]
    code: Option<Arc<str>>,
}

/// What a slide on a page is for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "slide_role")]
pub(crate) enum SlideRole {
    /// Played and stepped through.
    #[default]
    #[serde(rename = "live")]
    Live,
    /// Played and stepped through; its cues show on its notes copy instead.
    #[serde(rename = "live_beside_notes")]
    LiveBesideNotes,
    /// A still copy in the notes column, beside its live slide.
    #[serde(rename = "notes_copy")]
    NotesCopy,
}

impl SlideRole {
    fn is_live(&self) -> bool {
        *self == Self::Live
    }
}

/// A still frame of a slide: an SVG document of the slide's pixel size.
#[derive(Debug, Clone)]
pub(crate) struct StillFrame {
    pub(crate) svg: String,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl SlideSource {
    /// Read a ManimGX take of scene `name`, whose file holds `code`; it shows its end state.
    pub(crate) fn from_take(take: &[u8], name: String, code: String) -> anyhow::Result<Self> {
        let slide = take::slide_from_take(take)?;
        Ok(Self {
            take: Some(
                base64::engine::general_purpose::STANDARD
                    .encode(take)
                    .into(),
            ),
            scene: Scene {
                name,
                code: Some(code.into()),
            },
            time: slide.duration,
            file: None,
            role: SlideRole::Live,
            hash: None,
            converted: Some(Arc::new(slide)),
        })
    }

    /// The slide as the player shows it.
    fn slide(&self) -> anyhow::Result<Arc<Slide>> {
        if let Some(converted) = &self.converted {
            return Ok(Arc::clone(converted));
        }

        let take = self
            .take
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("a notes copy keeps only its still frame"))?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(take.as_bytes())?;
        Ok(Arc::new(take::slide_from_take(&bytes)?))
    }

    /// The code of the scene file the take was made from.
    #[cfg(test)]
    pub(crate) fn scene_code(&self) -> Option<&str> {
        self.scene.code.as_deref()
    }

    /// The scene's class name, which identifies the slide across renders.
    pub(crate) fn name(&self) -> &str {
        &self.scene.name
    }

    /// Remember the scene file the slide was made from.
    pub(crate) fn with_file(mut self, file: Option<PathBuf>) -> Self {
        self.file = file;
        self
    }

    pub(crate) fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Remember the hash of what made the take.
    pub(crate) fn with_hash(mut self, hash: Option<String>) -> Self {
        self.hash = hash;
        self
    }

    pub(crate) fn hash(&self) -> Option<&str> {
        self.hash.as_deref()
    }

    /// This slide beside its notes copy, which shows the cues instead of it.
    pub(crate) fn beside_notes(mut self) -> Self {
        self.role = SlideRole::LiveBesideNotes;
        self
    }

    /// Whether the lecturer sees the slide's cues on it: everywhere but beside a notes copy.
    pub(crate) fn cues(&self) -> Cues {
        if self.role == SlideRole::LiveBesideNotes {
            return Cues::Hidden;
        }
        Cues::Shown
    }

    /// This slide as a still copy for the notes column.
    pub(crate) fn into_notes_copy(mut self) -> Self {
        self.role = SlideRole::NotesCopy;
        self
    }

    pub(crate) fn is_notes_copy(&self) -> bool {
        self.role == SlideRole::NotesCopy
    }

    /// This slide placed like `other`, e.g. when it replaces it: a notes copy stays one.
    pub(crate) fn placed_like(mut self, other: &Self) -> Self {
        if other.is_notes_copy() {
            self.role = SlideRole::NotesCopy;
        }
        self
    }

    /// Whether `other` is this very take, not just equal to it.
    fn is_same_take(&self, other: &Arc<str>) -> bool {
        self.take
            .as_ref()
            .is_some_and(|take| Arc::ptr_eq(take, other))
    }

    /// The slide as a document keeps it, its still frame drawn: a notes copy without its
    /// take and code.
    pub(crate) fn into_stored(mut self) -> Self {
        self.converted = None;
        if self.is_notes_copy() {
            self.take = None;
            self.scene.code = None;
        }
        self
    }

    /// The slide at its shown time, drawn once.
    pub(crate) fn still_frame(&self) -> anyhow::Result<StillFrame> {
        let slide = self.slide()?;
        Ok(StillFrame {
            svg: slide.still_svg(self.time, self.cues())?,
            width: slide.frame.pw,
            height: slide.frame.ph,
        })
    }
}

/// A stored slide, or `None` for one this version cannot play, e.g. a slide of the
/// earlier slide-file format: its image keeps showing the still frame.
pub(crate) fn deserialize_lenient<'de, D>(deserializer: D) -> Result<Option<SlideSource>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}
