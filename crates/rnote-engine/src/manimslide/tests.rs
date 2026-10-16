//! Compare the port against fixtures of Manim's reference evaluator
//! (slide-format.md, section 11).
//!
//! `tests/data/manimslide` holds pairs `X.slide.json` + `X.fixture.json.gz`.
//! Set `RNOTE_MANIM_FIXTURES` to a folder of such pairs to check more slides.

use super::draw::{DrawNode, DrawSubpath, Line, Paint};
use super::timeline::{Cap, Join, Slide};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Tolerances of slide-format.md, section 11.
const COORD_TOL: f64 = 2e-4;
const COLOR_TOL: f64 = 1e-3;
const WIDTH_TOL: f64 = 1e-6;

const FIXTURE_SUFFIX: &str = ".fixture.json.gz";
const SLIDE_SUFFIX: &str = ".slide.json";
const FIXTURES_ENV: &str = "RNOTE_MANIM_FIXTURES";

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/manimslide")
}

/// Every `(slide file, fixture file)` pair in `dir`.
fn pairs(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut pairs: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| {
            let fixture = entry.ok()?.path();
            let name = fixture
                .file_name()?
                .to_str()?
                .strip_suffix(FIXTURE_SUFFIX)?
                .to_owned();
            Some((dir.join(name + SLIDE_SUFFIX), fixture))
        })
        .collect();
    pairs.sort();
    pairs
}

fn read_fixture(path: &Path) -> Value {
    let mut text = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn numbers(value: &Value) -> Vec<f64> {
    match value {
        Value::Array(items) => items.iter().flat_map(numbers).collect(),
        other => vec![other.as_f64().unwrap()],
    }
}

fn assert_close(what: &str, got: &[f64], expected: &[f64], tol: f64) {
    assert_eq!(got.len(), expected.len(), "{what}: length");
    for (i, (g, e)) in got.iter().zip(expected).enumerate() {
        assert!((g - e).abs() <= tol, "{what}[{i}]: got {g}, expected {e}");
    }
}

fn check_paint(what: &str, got: Option<&Paint>, expected: &Value) {
    match (got, expected) {
        (None, Value::Null) => {}
        (Some(Paint::Color(c)), e) if e.get("color").is_some() => {
            assert_close(what, c, &numbers(&e["color"]), COLOR_TOL);
        }
        (Some(Paint::Gradient { line, stops }), e) if e.get("gradient").is_some() => {
            assert_close(what, line, &numbers(&e["gradient"]), COORD_TOL);
            let stops: Vec<f64> = stops.iter().flatten().copied().collect();
            assert_close(what, &stops, &numbers(&e["stops"]), COLOR_TOL);
        }
        (got, e) => panic!("{what}: got {got:?}, expected {e}"),
    }
}

fn check_line(what: &str, got: Option<&Line>, expected: &Value) {
    match (got, expected) {
        (None, Value::Null) => {}
        (Some(line), e) if !e.is_null() => {
            assert_close(
                what,
                &[line.width],
                &[e["width"].as_f64().unwrap()],
                WIDTH_TOL,
            );
            check_paint(what, Some(&line.paint), &e["paint"]);
        }
        (got, e) => panic!("{what}: got {got:?}, expected {e}"),
    }
}

fn check_subpaths(what: &str, got: &[DrawSubpath], expected: &Value) {
    let expected = expected.as_array().unwrap();
    assert_eq!(got.len(), expected.len(), "{what}: subpath count");

    for (k, (sub, e)) in got.iter().zip(expected).enumerate() {
        let what = format!("{what} subpath {k}");
        assert_eq!(sub.closed, e["closed"].as_bool().unwrap(), "{what}: closed");
        assert_close(&what, &sub.start, &numbers(&e["move"]), COORD_TOL);
        let cubics: Vec<f64> = sub.cubics.iter().flatten().copied().collect();
        assert_close(&what, &cubics, &numbers(&e["cubics"]), COORD_TOL);
    }
}

fn join_name(join: Join) -> &'static str {
    match join {
        Join::Miter => "miter",
        Join::Round => "round",
        Join::Bevel => "bevel",
    }
}

fn cap_name(cap: Cap) -> &'static str {
    match cap {
        Cap::Butt => "butt",
        Cap::Round => "round",
        Cap::Square => "square",
    }
}

fn check_node(what: &str, got: &DrawNode, e: &Value) {
    match got {
        DrawNode::Path {
            node,
            subpaths,
            background,
            fill,
            stroke,
            join,
            cap,
        } => {
            assert_eq!(e["kind"], "path", "{what}");
            assert_eq!(*node as u64, e["node"].as_u64().unwrap(), "{what}: node");
            check_subpaths(what, subpaths, &e["subpaths"]);
            check_line(
                &format!("{what} background"),
                background.as_ref(),
                &e["background"],
            );
            check_paint(&format!("{what} fill"), fill.as_ref(), &e["fill"]);
            check_line(&format!("{what} stroke"), stroke.as_ref(), &e["stroke"]);
            assert_eq!(join_name(*join), e["join"], "{what}: join");
            assert_eq!(cap_name(*cap), e["cap"], "{what}: cap");
        }
        DrawNode::Image {
            node,
            image,
            corners,
            opacity,
            smooth,
        } => {
            assert_eq!(e["kind"], "image", "{what}");
            assert_eq!(*node as u64, e["node"].as_u64().unwrap(), "{what}: node");
            assert_eq!(*image as u64, e["image"].as_u64().unwrap(), "{what}: image");
            assert_close(what, corners, &numbers(&e["corners"]), COORD_TOL);
            assert_close(
                what,
                &[*opacity],
                &[e["opacity"].as_f64().unwrap()],
                COLOR_TOL,
            );
            assert_eq!(*smooth, e["smooth"].as_bool().unwrap(), "{what}: smooth");
        }
    }
}

fn check_pair(slide_path: &Path, fixture_path: &Path) {
    let slide = Slide::from_json(&std::fs::read_to_string(slide_path).unwrap()).unwrap();
    let fixture = read_fixture(fixture_path);
    let name = slide_path.file_name().unwrap().to_string_lossy();

    for frame in fixture["frames"].as_array().unwrap() {
        let t = frame["t"].as_f64().unwrap();
        let got: Vec<DrawNode> = slide
            .evaluate(t)
            .iter()
            .map(|(node, state)| DrawNode::new(*node, state))
            .collect();
        let expected = frame["nodes"].as_array().unwrap();

        let got_ids: Vec<_> = got
            .iter()
            .map(|n| match n {
                DrawNode::Path { node, .. } | DrawNode::Image { node, .. } => *node as u64,
            })
            .collect();
        let expected_ids: Vec<_> = expected
            .iter()
            .map(|e| e["node"].as_u64().unwrap())
            .collect();
        assert_eq!(got_ids, expected_ids, "{name} t={t}: nodes in draw order");

        for (node, e) in got.iter().zip(expected) {
            check_node(&format!("{name} t={t} node {}", e["node"]), node, e);
        }
    }
}

#[test]
fn matches_reference_fixtures() {
    let pairs = pairs(&data_dir());
    assert!(!pairs.is_empty(), "no fixtures found");
    for (slide, fixture) in pairs {
        check_pair(&slide, &fixture);
    }
}

/// More slides, e.g. a whole deck's fixtures, when the folder is given.
#[test]
fn matches_external_fixtures() {
    let Ok(dir) = std::env::var(FIXTURES_ENV) else {
        return;
    };
    for (slide, fixture) in pairs(Path::new(&dir)) {
        check_pair(&slide, &fixture);
    }
}

#[test]
fn rejects_unknown_version() {
    let text = r#"{"version": 2, "z": "", "images": []}"#;
    assert!(Slide::from_json(text).is_err());
}
