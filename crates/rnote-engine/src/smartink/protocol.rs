//! Wire format between Rnote and an external ink refiner.
//!
//! One JSON object per line in each direction.
//! Reference: `exploration/smart-ink/protocol.md`.

// Imports
use serde::{Deserialize, Serialize};

/// Bump on incompatible changes.
pub const PROTOCOL_VERSION: u32 = 0;

/// One trajectory sample. On the wire: `[x, y, pressure]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(from = "[f64; 3]", into = "[f64; 3]")]
pub struct InkPoint {
    pub x: f64,
    pub y: f64,
    /// Range [0.0, 1.0].
    pub pressure: f64,
}

impl From<[f64; 3]> for InkPoint {
    fn from([x, y, pressure]: [f64; 3]) -> Self {
        Self { x, y, pressure }
    }
}

impl From<InkPoint> for [f64; 3] {
    fn from(p: InkPoint) -> Self {
        [p.x, p.y, p.pressure]
    }
}

/// One pen-down .. pen-up trajectory plus rendering hints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InkStroke {
    pub points: Vec<InkPoint>,
    pub width: f64,
    /// RGBA, each in [0.0, 1.0].
    pub color: [f64; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefineRequest {
    pub version: u32,
    pub id: u64,
    /// 0.0 = untouched, 1.0 = maximal regularization.
    pub strength: f64,
    /// Chronological order.
    pub strokes: Vec<InkStroke>,
}

impl RefineRequest {
    pub fn new(id: u64, strength: f64, strokes: Vec<InkStroke>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            strength,
            strokes,
        }
    }
}

/// Strokes saved to disk for dataset collection. Same stroke format as requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InkDump {
    pub version: u32,
    /// Seconds since the Unix epoch, when the dump was taken.
    pub created: u64,
    pub strokes: Vec<InkStroke>,
}

impl InkDump {
    pub fn new(strokes: Vec<InkStroke>) -> Self {
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();

        Self {
            version: PROTOCOL_VERSION,
            created,
            strokes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefineResponse {
    pub version: u32,
    pub id: u64,
    #[serde(default)]
    pub strokes: Vec<InkStroke>,
    #[serde(default)]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke() -> InkStroke {
        InkStroke {
            points: vec![[0.0, 1.0, 0.5].into(), [2.0, 3.0, 0.7].into()],
            width: 2.0,
            color: [0.0, 0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn point_is_an_array_on_the_wire() {
        let json = serde_json::to_string(&InkPoint::from([1.0, 2.0, 0.5])).unwrap();
        assert_eq!(json, "[1.0,2.0,0.5]");
    }

    #[test]
    fn request_round_trip() {
        let request = RefineRequest::new(7, 0.3, vec![stroke()]);
        let json = serde_json::to_string(&request).unwrap();
        let back: RefineRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, request);
    }

    #[test]
    fn response_without_strokes_is_valid() {
        let back: RefineResponse =
            serde_json::from_str(r#"{"version":0,"id":1,"error":"boom"}"#).unwrap();
        assert!(back.strokes.is_empty());
        assert_eq!(back.error.as_deref(), Some("boom"));
    }
}
