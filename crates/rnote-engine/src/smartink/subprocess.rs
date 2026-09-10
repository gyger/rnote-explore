//! Refiner talking to a long-lived child process over JSON lines.
//!
//! ```text
//!  SubprocessRefiner ──stdin──► child (e.g. `uv run refiner serve`)
//!                    ◄─stdout── one JSON line per request
//!                       ▲
//!            reader thread + channel, so waiting can time out
//! ```
//!
//! Any I/O error or timeout kills the child; the next call respawns it.

// Imports
use super::Refiner;
use super::protocol::{InkStroke, PROTOCOL_VERSION, RefineRequest, RefineResponse};
use anyhow::{Context, bail};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;
use tracing::{debug, warn};

/// Longer waits are treated as a hung refiner.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct SubprocessRefiner {
    /// Program followed by its arguments.
    command: Vec<String>,
    session: Option<Session>,
    next_id: u64,
}

impl SubprocessRefiner {
    /// `command_line` is split on whitespace; quoting is not supported.
    pub fn new(command_line: &str) -> anyhow::Result<Self> {
        let command: Vec<String> = command_line.split_whitespace().map(str::to_owned).collect();
        if command.is_empty() {
            bail!("empty refiner command line");
        }

        Ok(Self {
            command,
            session: None,
            next_id: 0,
        })
    }

    fn session(&mut self) -> anyhow::Result<&mut Session> {
        if self.session.is_none() {
            self.session = Some(Session::spawn(&self.command)?);
        }

        self.session
            .as_mut()
            .context("refiner session missing after spawn")
    }

    fn exchange(&mut self, request: &RefineRequest) -> anyhow::Result<RefineResponse> {
        let result = self.session()?.exchange(request);

        if result.is_err() {
            warn!("refiner failed, dropping child process");
            self.session = None;
        }

        result
    }
}

impl Refiner for SubprocessRefiner {
    fn refine(&mut self, strength: f64, strokes: Vec<InkStroke>) -> anyhow::Result<Vec<InkStroke>> {
        let id = self.next_id;
        self.next_id += 1;

        let response = self.exchange(&RefineRequest::new(id, strength, strokes))?;
        if let Some(error) = response.error {
            bail!("refiner reported: {error}");
        }

        Ok(response.strokes)
    }
}

/// A running child with its stdin and a thread forwarding stdout lines.
#[derive(Debug)]
struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<std::io::Result<String>>,
}

impl Session {
    fn spawn(command: &[String]) -> anyhow::Result<Self> {
        let (program, args) = command.split_first().context("empty refiner command")?;

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning refiner `{program}`"))?;
        let stdin = child.stdin.take().context("refiner stdin missing")?;
        let stdout = child.stdout.take().context("refiner stdout missing")?;

        // Forward lines through a channel so the caller can wait with a timeout.
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        debug!("spawned refiner `{program}`");

        Ok(Self {
            child,
            stdin,
            lines,
        })
    }

    fn exchange(&mut self, request: &RefineRequest) -> anyhow::Result<RefineResponse> {
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;

        let line = match self.lines.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(line) => line?,
            Err(RecvTimeoutError::Timeout) => bail!("refiner timed out after {RESPONSE_TIMEOUT:?}"),
            Err(RecvTimeoutError::Disconnected) => bail!("refiner closed stdout"),
        };
        let response: RefineResponse =
            serde_json::from_str(&line).context("parsing refiner response")?;

        if response.version != PROTOCOL_VERSION {
            bail!(
                "refiner protocol v{} != v{PROTOCOL_VERSION}",
                response.version
            );
        }
        if response.id != request.id {
            bail!("refiner answered id {} to {}", response.id, request.id);
        }

        Ok(response)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_command_is_rejected() {
        assert!(SubprocessRefiner::new("   ").is_err());
    }

    /// Round trip through the real refiner. Run with:
    /// `RNOTE_SMARTINK_CMD="uv run --project <refiner dir> refiner serve" cargo test -- --ignored`
    #[test]
    #[ignore]
    fn real_refiner_round_trip() {
        let command_line = std::env::var(super::super::REFINER_CMD_ENV).unwrap();
        let mut refiner = SubprocessRefiner::new(&command_line).unwrap();
        let zigzag = InkStroke {
            points: (0..6)
                .map(|i| [f64::from(i), f64::from(i % 2), 0.5].into())
                .collect(),
            width: 2.0,
            color: [0.0, 0.0, 0.0, 1.0],
        };

        let first = refiner.refine(0.5, vec![zigzag.clone()]).unwrap();
        let second = refiner.refine(0.0, vec![zigzag.clone()]).unwrap();

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].points.len(), zigzag.points.len());
        assert_ne!(first[0].points, zigzag.points);
        assert_eq!(second[0].points, zigzag.points);
        assert!(refiner.session.is_some());
    }

    #[test]
    fn missing_program_fails_on_first_use() {
        let mut refiner = SubprocessRefiner::new("rnote-no-such-refiner-binary").unwrap();
        assert!(refiner.refine(0.5, vec![]).is_err());
        assert!(refiner.session.is_none());
    }
}
