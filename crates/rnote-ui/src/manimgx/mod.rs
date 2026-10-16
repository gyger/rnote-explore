//! Running ManimGX on a scene file: Python is started only to import or update slides.
//!
//! ```text
//!  scene.py ──uv run --with manimgx python record_takes.py──► NNNN_<Scene>.take ──► ManimScene
//! ```

use gettextrs::gettext;
use rnote_engine::engine::import::{ManimScene, SceneTake};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The command that runs a Python with ManimGX. uv fetches ManimGX on first use.
const PYTHON: [&str; 6] = [
    "uv",
    "run",
    "--no-project",
    "--with",
    "manimgx==0.1.1",
    "python",
];
/// Where to get uv, for the warning when it is missing.
const UV_URL: &str = "https://docs.astral.sh/uv/";
/// The driver Python runs; written next to the takes for each run.
const DRIVER: &str = include_str!("record_takes.py");
const DRIVER_NAME: &str = "record_takes.py";
const TAKE_EXTENSION: &str = "take";
/// Beside each take: the hash of what made it.
const HASH_EXTENSION: &str = "hash";
/// The hashes of the takes Rnote already has, for the driver to skip unchanged slides.
const KNOWN_NAME: &str = "known.json";
/// Lines of Python's error output shown when a run fails.
const ERROR_LINES: usize = 12;
/// Where Python's error output goes during a run, read if it fails.
const STDERR_NAME: &str = "stderr.txt";

/// A progress line of the driver (`record_takes.py`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Progress {
    /// The number of scenes in the file.
    Scenes(usize),
    /// Scene `index` (from 1) begins.
    Scene { index: usize, name: String },
    /// The scene's play `index` (from 1) ended.
    Play { index: usize },
    /// Scene `index` (from 1) is unchanged and not run.
    Skip { index: usize, name: String },
}

impl Progress {
    /// Example: `scene 2 Doppelspalt`. Other lines, e.g. a scene's own prints, give `None`.
    fn parse(line: &str) -> Option<Self> {
        let mut words = line.split_whitespace();
        let kind = words.next()?;
        let index = words.next()?.parse().ok()?;
        match kind {
            "scenes" => Some(Self::Scenes(index)),
            "scene" => Some(Self::Scene {
                index,
                name: words.next()?.to_owned(),
            }),
            "play" => Some(Self::Play { index }),
            "skip" => Some(Self::Skip {
                index,
                name: words.next()?.to_owned(),
            }),
            _ => None,
        }
    }
}

/// Where a run is, put together from its progress lines.
#[derive(Debug, Clone, Default)]
pub(crate) struct Status {
    scenes: usize,
    scene: usize,
    name: String,
    play: usize,
    unchanged: usize,
    /// Whether the current scene is skipped.
    skipping: bool,
}

impl Status {
    pub(crate) fn update(&mut self, progress: Progress) {
        match progress {
            Progress::Scenes(count) => self.scenes = count,
            Progress::Scene { index, name } => {
                (self.scene, self.name, self.play, self.skipping) = (index, name, 0, false);
            }
            Progress::Play { index } => self.play = index,
            Progress::Skip { index, name } => {
                (self.scene, self.name, self.play, self.skipping) = (index, name, 0, true);
                self.unchanged += 1;
            }
        }
    }

    /// The scenes skipped as unchanged.
    pub(crate) fn unchanged(&self) -> usize {
        self.unchanged
    }

    /// Example: "scene 2 of 3: Doppelspalt, animation 7".
    pub(crate) fn text(&self) -> String {
        if self.scene == 0 {
            return gettext("starting");
        }
        let mut text = gettext("scene {} of {}: {}")
            .replacen("{}", &self.scene.to_string(), 1)
            .replacen("{}", &self.scenes.to_string(), 1)
            .replacen("{}", &self.name, 1);
        if self.skipping {
            text += &gettext(", unchanged");
        }
        if self.play > 0 {
            text += &gettext(", animation {}").replace("{}", &self.play.to_string());
        }
        text
    }
}

/// Run the scene file at `path` and collect the take of each of its scenes, telling
/// `on_progress` how far it is. Scenes whose hash is in `known` and unchanged are skipped.
///
/// Blocks until ManimGX is done; call it off the main thread.
pub(crate) fn record_scene(
    path: &Path,
    known: &HashMap<String, String>,
    on_progress: impl Fn(Progress),
) -> anyhow::Result<ManimScene> {
    let code = std::fs::read_to_string(path)?;
    let dir = run_dir()?;
    let result = record_in(path, known, &dir, on_progress).map(|takes| ManimScene {
        path: path.to_path_buf(),
        code,
        takes,
    });
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A fresh folder for one run's driver and takes.
fn run_dir() -> anyhow::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("rnote-manimgx-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn record_in(
    path: &Path,
    known: &HashMap<String, String>,
    dir: &Path,
    on_progress: impl Fn(Progress),
) -> anyhow::Result<Vec<SceneTake>> {
    let driver = dir.join(DRIVER_NAME);
    std::fs::write(&driver, DRIVER)?;
    let known_path = dir.join(KNOWN_NAME);
    std::fs::write(&known_path, serde_json::to_string(known)?)?;
    // Error output goes to a file: a pipe nobody reads could fill and stall Python.
    let stderr_path = dir.join(STDERR_NAME);

    let mut command = Command::new(PYTHON[0]);
    command
        .args(&PYTHON[1..])
        .arg(&driver)
        .arg(path)
        .arg(dir)
        .arg(&known_path)
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&stderr_path)?);
    hide_console(&mut command);
    let mut child = command.spawn().map_err(start_error)?;

    // Progress, line by line, as the scenes run.
    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout).lines() {
            if let Some(progress) = Progress::parse(&line?) {
                on_progress(progress);
            }
        }
    }

    if !child.wait()?.success() {
        let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
        let tail: Vec<&str> = stderr.lines().rev().take(ERROR_LINES).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        anyhow::bail!("ManimGX failed on {}:\n{}", path.display(), tail.join("\n"));
    }
    takes_in(dir)
}

/// Why Python did not start. Without uv, the user learns what it is for and where to get it.
fn start_error(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        return anyhow::anyhow!(
            "{}",
            gettext(
                "Rnote runs ManimGX through uv, which is not installed. Install it from {} and restart Rnote."
            )
            .replace("{}", UV_URL)
        );
    }
    anyhow::anyhow!("starting `{}` failed: {e}", PYTHON.join(" "))
}

/// The takes in `dir`, in the scene file's order (their names start with the index).
fn takes_in(dir: &Path) -> anyhow::Result<Vec<SceneTake>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|e| e == TAKE_EXTENSION))
        .collect();
    files.sort();

    files
        .into_iter()
        .map(|file| {
            // Example: "0002_Doppelspalt.take" holds scene "Doppelspalt".
            let stem = file.file_stem().unwrap_or_default().to_string_lossy();
            let name = stem
                .split_once('_')
                .map_or(stem.as_ref(), |(_, n)| n)
                .to_owned();
            Ok(SceneTake {
                name,
                take: std::fs::read(&file)?,
                hash: std::fs::read_to_string(file.with_extension(HASH_EXTENSION)).ok(),
            })
        })
        .collect()
}

/// Python is a console program; from a window it must not flash a console up.
#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_driver_progress_lines() {
        assert_eq!(Progress::parse("scenes 3"), Some(Progress::Scenes(3)));
        assert_eq!(
            Progress::parse("scene 2 Doppelspalt"),
            Some(Progress::Scene {
                index: 2,
                name: "Doppelspalt".to_owned()
            })
        );
        assert_eq!(
            Progress::parse("play 7 12.5"),
            Some(Progress::Play { index: 7 })
        );
        assert_eq!(
            Progress::parse("skip 3 Euler"),
            Some(Progress::Skip {
                index: 3,
                name: "Euler".to_owned()
            })
        );
        // Anything else a scene prints is not progress.
        assert_eq!(Progress::parse("hello from the scene"), None);
    }

    #[test]
    fn progress_reads_as_one_line() {
        let mut status = Status::default();
        for line in ["scenes 3", "scene 2 Doppelspalt", "play 7 12.5"] {
            status.update(Progress::parse(line).unwrap());
        }
        assert_eq!(status.text(), "scene 2 of 3: Doppelspalt, animation 7");
    }

    #[test]
    fn missing_uv_says_how_to_get_it() {
        let missing = start_error(std::io::ErrorKind::NotFound.into()).to_string();
        assert!(missing.contains(UV_URL), "{missing}");

        let other = start_error(std::io::ErrorKind::PermissionDenied.into()).to_string();
        assert!(other.starts_with("starting `uv run"), "{other}");
    }

    #[test]
    fn skipped_scenes_are_counted() {
        let mut status = Status::default();
        for line in ["scenes 3", "skip 1 Titel", "skip 2 Euler"] {
            status.update(Progress::parse(line).unwrap());
        }
        assert_eq!(status.text(), "scene 2 of 3: Euler, unchanged");
        status.update(Progress::parse("scene 3 Drehen").unwrap());
        assert_eq!(status.text(), "scene 3 of 3: Drehen");
        assert_eq!(status.unchanged(), 2);
    }
}
