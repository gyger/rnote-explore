//! Smart Ink: hand selected strokes to a refiner and take back replacement strokes.
//!
//! ```text
//! Engine::refine_selection
//!         │  Vec<InkStroke>
//!         ▼
//!    dyn Refiner ──► SubprocessRefiner   JSON lines over stdin/stdout
//!               └──► PassthroughRefiner  identity, for tests
//!         │
//!         ▼
//!      protocol                          serde wire structs
//! ```
//!
//! The engine never touches process I/O; it only sees the [Refiner] trait.

// Modules
pub mod convert;
pub mod passthrough;
pub mod protocol;
pub mod subprocess;

// Imports
use protocol::InkStroke;
use tracing::{info, warn};

/// Environment variable holding the refiner command line, e.g.
/// `uv run --project <path> refiner serve`.
pub const REFINER_CMD_ENV: &str = "RNOTE_SMARTINK_CMD";

/// Regularization applied when the UI has no slider yet. 0.0 = untouched, 1.0 = maximal.
pub const DEFAULT_STRENGTH: f64 = 0.3;

/// Something that turns a group of strokes into a cleaner group of strokes.
///
/// Input and output counts may differ (merging or splitting strokes is allowed).
pub trait Refiner: std::fmt::Debug + Send {
    fn refine(&mut self, strength: f64, strokes: Vec<InkStroke>) -> anyhow::Result<Vec<InkStroke>>;
}

/// Build the refiner configured through [REFINER_CMD_ENV]. `None` when unset or invalid.
pub fn from_env() -> Option<Box<dyn Refiner>> {
    let command_line = std::env::var(REFINER_CMD_ENV).ok()?;

    match subprocess::SubprocessRefiner::new(&command_line) {
        Ok(refiner) => {
            info!("smart ink refiner: `{command_line}`");
            Some(Box::new(refiner))
        }
        Err(e) => {
            warn!("ignoring {REFINER_CMD_ENV}=`{command_line}`, Err: {e:?}");
            None
        }
    }
}
