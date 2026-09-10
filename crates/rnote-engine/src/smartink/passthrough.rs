//! Identity refiner. Exercises the replace/undo path without an external process.

// Imports
use super::Refiner;
use super::protocol::InkStroke;

#[derive(Debug, Default)]
pub struct PassthroughRefiner;

impl Refiner for PassthroughRefiner {
    fn refine(
        &mut self,
        _strength: f64,
        strokes: Vec<InkStroke>,
    ) -> anyhow::Result<Vec<InkStroke>> {
        Ok(strokes)
    }
}
