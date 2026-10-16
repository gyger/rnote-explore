//! Manim vector slides: reading and evaluating `.slide.json` files.
//!
//! The format is specified in the Manim fork,
//! `manim/renderer/vector/slide-format.md`; `SlideTimeline.evaluate` there is
//! the reference this port is tested against.
//!
//! ```text
//!  .slide.json ──format──► Slide ──evaluate(t)──► states ──draw──► DrawNode list
//!   (packed)                (tracks, states,       (blend, arc,       (paths and paint,
//!                            eases, draw order)     partial ops)       Manim units, y up)
//! ```

// Not used by the engine yet: the importer and the renderer come next.
#![allow(dead_code)]

mod draw;
mod format;
mod ops;
mod render;
mod timeline;

#[cfg(test)]
mod tests;
