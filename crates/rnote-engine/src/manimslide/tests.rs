//! Tests against ManimGX takes in `tests/data/manimslide/take`, all ManimGX 0.1.1 at
//! 480x270 and 15 fps (recorded by `exploration/manimgx-slides`):
//!
//! ```text
//!  spike.take    section "shapes" (Create a square, Transform it into a circle),
//!                section "text" (Write a text, move and recolor it); 76 frames
//!  spike@F.png   frame F as ManimGX's own player draws it (Replay.render)
//!  parts.take    sections "one", "two" (presentation.loop), "three", 1 s each
//!  single.take   one section: Create a square, 1 s
//!  cue.take      Create a square (left) and a cue circle (right), 1 s
//!  image.take    a gradient picture fades in (1 s), then moves and shrinks (1 s)
//!  image@F.png   frame F as ManimGX's own player draws it
//! ```

use super::render::Cues;
use super::timeline::{Seen, Slide};
use super::{Position, SlidePlayer, SlideSource, Step};
use crate::store::StrokeKey;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const TAKE_FPS: f64 = 15.0;
/// A channel this far off counts the pixel as different (antialiasing differs slightly).
const PIXEL_TOL: i16 = 24;
/// Share of pixels allowed to differ from ManimGX's picture: a few antialiased edge pixels
/// (measured: at most 0.01%). Kept small, because thin strokes cover few pixels: the half of a
/// square missing mid-Create is 0.2% of the picture.
const DIFFERING_SHARE: f64 = 0.001;

fn take_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/manimslide/take")
}

fn take(name: &str) -> Vec<u8> {
    std::fs::read(take_dir().join(format!("{name}.take"))).unwrap()
}

fn slide(name: &str) -> Slide {
    super::take::slide_from_take(&take(name)).unwrap()
}

fn render(slide: &Slide, t: f64, cues: Cues) -> cairo::ImageSurface {
    let (w, h) = (slide.frame.pw as i32, slide.frame.ph as i32);
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();

    let cairo_cx = cairo::Context::new(&surface).unwrap();
    let mut cx = piet_cairo::CairoRenderContext::new(&cairo_cx);
    let target = kurbo::Rect::new(0.0, 0.0, w as f64, h as f64);
    slide.draw_at(&mut cx, t, target, cues).unwrap();
    piet::RenderContext::finish(&mut cx).unwrap();
    drop(cx);
    drop(cairo_cx);
    surface
}

/// The frame of `<take>@F.png`, for references of take `take`.
fn reference_frame(path: &Path, take: &str) -> Option<f64> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".png")?;
    let (name, frame) = stem.rsplit_once('@')?;
    (name == take).then_some(())?;
    frame.parse().ok()
}

/// Compare take `name` with each of its reference pictures; the number compared.
fn compare_references(name: &str) -> usize {
    let slide = slide(name);
    let mut checked = 0;
    for entry in std::fs::read_dir(take_dir()).unwrap() {
        let path = entry.unwrap().path();
        let Some(frame) = reference_frame(&path, name) else {
            continue;
        };
        let share = differing_share(&slide, frame / TAKE_FPS, &path);
        assert!(
            share <= DIFFERING_SHARE,
            "{name} frame {frame}: {:.2}% of pixels differ",
            share * 100.0
        );
        checked += 1;
    }
    checked
}

/// Share of pixels of `slide` at `t` that differ from the reference image at `path`.
fn differing_share(slide: &Slide, t: f64, path: &Path) -> f64 {
    let mut surface = render(slide, t, Cues::Shown);
    let expected = image::open(path).unwrap().into_rgba8();
    assert_eq!(
        (surface.width() as u32, surface.height() as u32),
        expected.dimensions(),
        "{}: size",
        path.display()
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
    differing as f64 / (expected.width() * expected.height()) as f64
}

// -- reading and drawing -------------------------------------------------------

#[test]
fn reads_take_timing_and_sections() {
    let slide = slide("spike");
    assert_eq!((slide.frame.pw, slide.frame.ph), (480.0, 270.0));
    assert!((slide.duration - 76.0 / TAKE_FPS).abs() < 1e-9);
    // Two sections, so one stop where "text" begins.
    assert_eq!(slide.stops.len(), 1);
    assert!(slide.stops[0] > 0.0 && slide.stops[0] < slide.duration);
    assert!(slide.loops.is_empty());
}

#[test]
fn reads_looped_sections_as_looped_parts() {
    let slide = slide("parts");
    assert_eq!(slide.stops, vec![1.0, 2.0]);
    assert_eq!(slide.loops, vec![1]);
}

#[test]
fn rejects_unknown_take_versions() {
    // START is the first message: length u32, opcode u8, then the version u32.
    let mut bytes = take("spike");
    bytes[5..9].copy_from_slice(&2u32.to_le_bytes());
    assert!(super::take::slide_from_take(&bytes).is_err());
}

#[test]
fn take_renders_like_manimgx() {
    assert_eq!(compare_references("spike"), 4);
}

#[test]
fn images_render_like_manimgx() {
    assert_eq!(compare_references("image"), 3);
}

// -- cues ------------------------------------------------------------------------

/// Pixels of `surface` in columns `[from, to)` that differ from its top-left pixel (the
/// background).
fn drawn_pixels(surface: &mut cairo::ImageSurface, from: i32, to: i32) -> usize {
    let (stride, height) = (surface.stride() as usize, surface.height() as usize);
    let data = surface.data().unwrap();
    let background = &data[0..4];
    (0..height)
        .flat_map(|y| (from as usize..to as usize).map(move |x| y * stride + x * 4))
        .filter(|&i| data[i..i + 4] != *background)
        .count()
}

#[test]
fn reads_cue_flags() {
    let slide = slide("cue");
    let seen = |s: Seen| slide.states.iter().any(|state| state.seen() == s);
    assert!(seen(Seen::ByAll));
    assert!(seen(Seen::ByLecturer));
}

#[test]
fn hidden_cues_are_not_drawn() {
    let slide = slide("cue");
    let mut shown = render(&slide, slide.duration, Cues::Shown);
    let mut hidden = render(&slide, slide.duration, Cues::Hidden);
    let (half, width) = (shown.width() / 2, shown.width());

    // The circle (a cue) is on the right, the square on the left.
    assert!(drawn_pixels(&mut shown, half, width) > 0);
    assert_eq!(drawn_pixels(&mut hidden, half, width), 0);
    assert_eq!(
        drawn_pixels(&mut shown, 0, half),
        drawn_pixels(&mut hidden, 0, half)
    );
    assert!(drawn_pixels(&mut hidden, 0, half) > 0);
}

// -- playing in parts ------------------------------------------------------------
//
// `parts`: 3 s, stops at 1 and 2 s, part 1 (1-2 s) loops.

fn parts_source() -> SlideSource {
    SlideSource::from_take(&take("parts"), "Parts".to_owned(), String::new()).unwrap()
}

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

fn assert_time(player: &SlidePlayer, key: StrokeKey, now: Instant, t: f64) {
    let got = player.time_at(key, now).unwrap();
    assert!((got - t).abs() < 1e-6, "time {got}, expected {t}");
}

/// A player that played `parts` from `start` to its first stop.
fn halted_at_first_stop() -> (SlidePlayer, StrokeKey, Instant) {
    let (mut player, key, start) = (SlidePlayer::default(), StrokeKey::default(), Instant::now());
    player.play_at(key, &parts_source(), start).unwrap();
    let _ = player.frame_at(start + secs(1.5));
    (player, key, start)
}

#[test]
fn playing_halts_at_the_next_stop() {
    let (player, key, start) = halted_at_first_stop();
    assert_eq!(player.position(key), Position::Middle);
    assert_time(&player, key, start + secs(1.5), 1.0);
}

#[test]
fn forward_while_playing_a_part_jumps_to_its_end() {
    let (mut player, key, start) = (SlidePlayer::default(), StrokeKey::default(), Instant::now());
    player.play_at(key, &parts_source(), start).unwrap();

    assert_eq!(player.forward_at(key, start + secs(0.5)), Step::Stay);
    assert_eq!(player.position(key), Position::Middle);
    assert_time(&player, key, start + secs(9.0), 1.0);
}

#[test]
fn looped_part_repeats_until_forward() {
    let (mut player, key, start) = halted_at_first_stop();
    let looping = start + secs(2.0);
    assert_eq!(player.forward_at(key, looping), Step::Stay);

    // 2.5 s later it is half way through its third round.
    let later = looping + secs(2.5);
    assert!(player.frame_at(later).playing);
    assert_time(&player, key, later, 1.5);

    // Forward ends the loop and plays on from its end.
    assert_eq!(player.forward_at(key, later), Step::Stay);
    assert_eq!(player.position(key), Position::Playing);
    assert_time(&player, key, later, 2.0);
}

#[test]
fn back_halts_at_the_previous_stop_then_leaves() {
    let (mut player, key, start) = halted_at_first_stop();
    let _ = player.forward_at(key, start + secs(2.0)); // the loop
    let _ = player.forward_at(key, start + secs(2.0)); // on from 2 s

    let now = start + secs(2.5); // 2.5 s into the slide
    assert_eq!(player.back_at(key, now), Step::Stay);
    assert_time(&player, key, now, 2.0);
    assert_eq!(player.back_at(key, now), Step::Stay);
    assert_time(&player, key, now, 1.0);
    assert_eq!(player.back_at(key, now), Step::Stay);
    assert_time(&player, key, now, 0.0);
    assert_eq!(player.back_at(key, now), Step::Leave);
}

#[test]
fn plain_vector_image_has_no_slide_field() {
    let json = serde_json::to_string(&crate::strokes::VectorImage::default()).unwrap();
    assert!(!json.contains("manim_slide"));
}
