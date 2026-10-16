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

// -- rendering ---------------------------------------------------------------
//
// `X@T.png` is slide X at time T, drawn by `exploration/manim-slides/slide_to_png.py`
// (pycairo, the same drawing rules) at the slide's pixel size.

/// A channel this far off counts the pixel as different (antialiasing differs slightly).
const PIXEL_TOL: i16 = 24;
/// Share of pixels allowed to differ: edges of upscaled images are filtered differently.
const DIFFERING_SHARE: f64 = 0.01;

fn render(slide: &Slide, t: f64) -> cairo::ImageSurface {
    let (w, h) = (slide.frame.pw as i32, slide.frame.ph as i32);
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();
    let images = super::render::SlideImages::decode(slide).unwrap();

    let cairo_cx = cairo::Context::new(&surface).unwrap();
    let mut cx = piet_cairo::CairoRenderContext::new(&cairo_cx);
    let target = kurbo::Rect::new(0.0, 0.0, w as f64, h as f64);
    slide.draw_at(&mut cx, &images, t, target).unwrap();
    piet::RenderContext::finish(&mut cx).unwrap();
    drop(cx);
    drop(cairo_cx);
    surface
}

/// `(slide name, time)` of `X@T.png`.
fn reference_name(path: &Path) -> Option<(String, f64)> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".png")?;
    let (name, t) = stem.rsplit_once('@')?;
    Some((name.to_owned(), t.parse().ok()?))
}

#[test]
fn renders_like_reference_images() {
    let mut checked = 0;
    for entry in std::fs::read_dir(data_dir()).unwrap() {
        let path = entry.unwrap().path();
        let Some((name, t)) = reference_name(&path) else {
            continue;
        };

        let text = std::fs::read_to_string(data_dir().join(name.clone() + SLIDE_SUFFIX)).unwrap();
        let slide = Slide::from_json(&text).unwrap();
        let mut surface = render(&slide, t);
        let expected = image::open(&path).unwrap().into_rgba8();
        assert_eq!(
            (surface.width() as u32, surface.height() as u32),
            expected.dimensions(),
            "{name}@{t}: size"
        );

        // Cairo stores premultiplied BGRA; the slides have opaque backgrounds.
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        let mut differing = 0;
        for (x, y, px) in expected.enumerate_pixels() {
            let i = y as usize * stride + x as usize * 4;
            let got = [data[i + 2], data[i + 1], data[i], data[i + 3]];
            if got
                .iter()
                .zip(px.0)
                .any(|(g, e)| (*g as i16 - e as i16).abs() > PIXEL_TOL)
            {
                differing += 1;
            }
        }

        let share = differing as f64 / (expected.width() * expected.height()) as f64;
        assert!(
            share <= DIFFERING_SHARE,
            "{name}@{t}: {:.2}% of pixels differ",
            share * 100.0
        );
        checked += 1;
    }
    assert!(checked > 0, "no reference images found");
}

// -- storage -----------------------------------------------------------------

#[test]
fn imported_slide_survives_saving() {
    let text = std::fs::read_to_string(data_dir().join("0001_paint.slide.json")).unwrap();
    let source = super::SlideSource::from_json(text.clone()).unwrap();
    let page = p2d::bounding_volume::Aabb::new(
        p2d::math::Vector2::new(0.0, 0.0),
        p2d::math::Vector2::new(1600.0, 1000.0),
    );
    let image = crate::strokes::VectorImage::from_manim_slide(source, page).unwrap();

    // Fitted into the page with the slide's aspect ratio, centered.
    let bounds = rnote_compose::shapes::Shapeable::bounds(&image);
    let slide = Slide::from_json(&text).unwrap();
    let aspect = slide.frame.pw / slide.frame.ph;
    assert!((bounds.extents()[0] / bounds.extents()[1] - aspect).abs() < 1e-6);
    assert!((bounds.mins[1] + bounds.maxs[1] - 1000.0).abs() < 1e-6);
    assert!(image.svg_data.contains("<path"), "still frame has no paths");

    // Saved and loaded again, the slide text is unchanged.
    let json = serde_json::to_string(&image).unwrap();
    let loaded: crate::strokes::VectorImage = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.manim_slide.unwrap().slide_json, text);
}

#[test]
fn plain_vector_image_has_no_slide_field() {
    let json = serde_json::to_string(&crate::strokes::VectorImage::default()).unwrap();
    assert!(!json.contains("manim_slide"));
}
