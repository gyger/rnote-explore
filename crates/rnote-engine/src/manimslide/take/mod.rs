//! ManimGX takes as slides.
//!
//! A take is ManimGX's record of a scene's run: its shapes once, then each frame's draw
//! records. Rnote stores the take as it comes and turns it into a slide when it loads, so
//! a better conversion improves every document, and no Manim of our own is needed:
//!
//! ```text
//!  scene.py ──ManimGX (Python, on import/update only)──► take ──read──► Take ──convert──► Slide
//! ```

mod convert;
mod read;

use super::timeline::Slide;

/// The slide a take shows.
pub(crate) fn slide_from_take(bytes: &[u8]) -> anyhow::Result<Slide> {
    let take = read::Take::read(bytes)?;
    convert::slide_of(&take)
}
