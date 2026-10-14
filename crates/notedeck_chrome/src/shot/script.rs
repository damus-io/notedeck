//! The command line of a shot: which flags script the run, in order, and
//! which are left for notedeck itself.
//!
//! A shot is notedeck run with `--shot <out.png>` among its arguments. The
//! shot's flags are steps, taken in the order they are written:
//!
//! - `--wait <secs>`: run frames for that long (`1.5`, `1.5s`, `500ms`), so
//!   async work lands: ndb ingest, relay traffic, animations.
//! - `--frames <n>`: run that many frames.
//! - `--key <keys>`: press keys, in the keymap's syntax (`<C-k>`, `g g`,
//!   `<Enter>`; see [`crate::keymap::keys`]).
//! - `--type <text>`: type text into whatever has focus.
//! - `--click <x>,<y>`: click there, in the window's (and the PNG's) pixels.
//! - `--hover <x>,<y>`: move the pointer there.
//! - `--shot <out.png>`: write what the window shows now.
//!
//! `--size <W>x<H>` (default [`DEFAULT_SIZE`]) sizes the window, wherever it
//! is written. Every other argument is notedeck's own (`--datapath`, `--sec`,
//! `-r`…) and is passed on untouched.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::keymap::{KeyParseError, KeySeq};

/// The window size when `--size` is not given: the live rig's screen.
pub const DEFAULT_SIZE: ShotSize = ShotSize {
    width: 1600,
    height: 1000,
};

/// The flag that asks for a shot, and writes one.
const SHOT_FLAG: &str = "--shot";

/// A window size in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShotSize {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// One scripted step of a shot.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Run frames for this long.
    Wait(Duration),
    /// Run this many frames.
    Frames(u32),
    /// Press these keys, one after another.
    Key(KeySeq),
    /// Type this text.
    Type(String),
    /// Click the primary button here, in pixels.
    Click(egui::Pos2),
    /// Move the pointer here, in pixels.
    Hover(egui::Pos2),
    /// Write the window to this PNG.
    Shot(PathBuf),
}

/// A shot's command line, split into its script and notedeck's own args.
#[derive(Debug, PartialEq)]
pub struct ShotPlan {
    /// The window size.
    pub size: ShotSize,
    /// The steps, in the order they were written. The last is a
    /// [`Step::Shot`].
    pub steps: Vec<Step>,
    /// Every argument that isn't the shot's, in order, program name first.
    pub args: Vec<String>,
}

/// Why a shot's command line can't be run.
#[derive(Debug, PartialEq)]
pub enum ShotArgError {
    /// A flag was the last argument, with no value after it.
    MissingValue(&'static str),
    /// `--size` wasn't `<W>x<H>`.
    BadSize(String),
    /// `--wait` wasn't a duration.
    BadWait(String),
    /// `--frames` wasn't a count.
    BadFrames(String),
    /// `--click` or `--hover` wasn't `<x>,<y>`.
    BadPoint(String),
    /// `--key` wasn't in the keymap's key syntax.
    BadKeys {
        /// What was written.
        text: String,
        /// What's wrong with it.
        err: KeyParseError,
    },
    /// Steps come after the last `--shot`, so nothing would show them.
    StepsAfterLastShot,
}

impl fmt::Display for ShotArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShotArgError::MissingValue(flag) => write!(f, "{flag} needs a value"),
            ShotArgError::BadSize(s) => write!(f, "--size wants <W>x<H>, got '{s}'"),
            ShotArgError::BadWait(s) => {
                write!(f, "--wait wants seconds (1.5, 1.5s, 500ms), got '{s}'")
            }
            ShotArgError::BadFrames(s) => write!(f, "--frames wants a count, got '{s}'"),
            ShotArgError::BadPoint(s) => write!(f, "--click/--hover want <x>,<y>, got '{s}'"),
            ShotArgError::BadKeys { text, err } => write!(f, "--key '{text}': {err}"),
            ShotArgError::StepsAfterLastShot => {
                write!(f, "steps after the last --shot would never be seen")
            }
        }
    }
}

impl std::error::Error for ShotArgError {}

impl ShotPlan {
    /// Read a shot out of a command line. `Ok(None)` when it has no
    /// `--shot`: then nothing is taken out of it, and notedeck runs as it
    /// always does.
    pub fn from_args(args: &[String]) -> Result<Option<ShotPlan>, ShotArgError> {
        let mut size = DEFAULT_SIZE;
        let mut steps = Vec::new();
        let mut rest = Vec::with_capacity(args.len());
        let mut iter = args.iter();

        while let Some(arg) = iter.next() {
            let flag: &'static str = match arg.as_str() {
                SHOT_FLAG => SHOT_FLAG,
                "--size" => "--size",
                "--wait" => "--wait",
                "--frames" => "--frames",
                "--key" => "--key",
                "--type" => "--type",
                "--click" => "--click",
                "--hover" => "--hover",
                _ => {
                    rest.push(arg.clone());
                    continue;
                }
            };
            let value = iter.next().ok_or(ShotArgError::MissingValue(flag))?;
            match flag {
                "--size" => size = parse_size(value)?,
                SHOT_FLAG => steps.push(Step::Shot(PathBuf::from(value))),
                "--wait" => steps.push(Step::Wait(parse_wait(value)?)),
                "--frames" => steps.push(Step::Frames(
                    value
                        .parse()
                        .map_err(|_| ShotArgError::BadFrames(value.clone()))?,
                )),
                "--key" => steps.push(Step::Key(KeySeq::parse(value).map_err(|err| {
                    ShotArgError::BadKeys {
                        text: value.clone(),
                        err,
                    }
                })?)),
                "--type" => steps.push(Step::Type(value.clone())),
                "--click" => steps.push(Step::Click(parse_point(value)?)),
                _ => steps.push(Step::Hover(parse_point(value)?)),
            }
        }

        let Some(last_shot) = steps.iter().rposition(|s| matches!(s, Step::Shot(_))) else {
            return Ok(None);
        };
        if last_shot + 1 != steps.len() {
            return Err(ShotArgError::StepsAfterLastShot);
        }
        Ok(Some(ShotPlan {
            size,
            steps,
            args: rest,
        }))
    }
}

/// `1600x1000`.
fn parse_size(s: &str) -> Result<ShotSize, ShotArgError> {
    let bad = || ShotArgError::BadSize(s.to_owned());
    let (w, h) = s.split_once('x').ok_or_else(bad)?;
    let width: u32 = w.parse().map_err(|_| bad())?;
    let height: u32 = h.parse().map_err(|_| bad())?;
    if width == 0 || height == 0 {
        return Err(bad());
    }
    Ok(ShotSize { width, height })
}

/// `1.5`, `1.5s` or `500ms`.
fn parse_wait(s: &str) -> Result<Duration, ShotArgError> {
    let bad = || ShotArgError::BadWait(s.to_owned());
    let secs: f64 = if let Some(ms) = s.strip_suffix("ms") {
        ms.parse::<f64>().map_err(|_| bad())? / 1000.0
    } else {
        s.strip_suffix('s')
            .unwrap_or(s)
            .parse()
            .map_err(|_| bad())?
    };
    Duration::try_from_secs_f64(secs).map_err(|_| bad())
}

/// `800,600`.
fn parse_point(s: &str) -> Result<egui::Pos2, ShotArgError> {
    let bad = || ShotArgError::BadPoint(s.to_owned());
    let (x, y) = s.split_once(',').ok_or_else(bad)?;
    let x: f32 = x.trim().parse().map_err(|_| bad())?;
    let y: f32 = y.trim().parse().map_err(|_| bad())?;
    Ok(egui::pos2(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_shot_flag_leaves_the_command_line_alone() {
        let plan = ShotPlan::from_args(&args(&["notedeck", "--key", "<C-k>", "--datapath", "d"]));
        assert_eq!(plan, Ok(None));
    }

    #[test]
    fn steps_run_in_written_order_and_the_rest_passes_through() {
        let plan = ShotPlan::from_args(&args(&[
            "notedeck",
            "--datapath",
            "d",
            "--wait",
            "500ms",
            "--key",
            "<C-k>",
            "--type",
            "headway",
            "--size",
            "800x600",
            "--click",
            "10, 20",
            "-r",
            "ws://127.0.0.1:9",
            "--shot",
            "out.png",
        ]))
        .unwrap()
        .unwrap();

        assert_eq!(
            plan.size,
            ShotSize {
                width: 800,
                height: 600
            }
        );
        assert_eq!(
            plan.steps,
            vec![
                Step::Wait(Duration::from_millis(500)),
                Step::Key(KeySeq::parse("<C-k>").unwrap()),
                Step::Type("headway".to_owned()),
                Step::Click(egui::pos2(10.0, 20.0)),
                Step::Shot(PathBuf::from("out.png")),
            ]
        );
        assert_eq!(
            plan.args,
            args(&["notedeck", "--datapath", "d", "-r", "ws://127.0.0.1:9"])
        );
    }

    #[test]
    fn a_value_is_never_read_as_a_flag() {
        // `--type --shot` types the text "--shot"; with no real --shot after
        // it there is nothing to shoot.
        let plan = ShotPlan::from_args(&args(&["notedeck", "--type", "--shot"]));
        assert_eq!(plan, Ok(None));
    }

    #[test]
    fn waits_read_seconds_with_or_without_units() {
        assert_eq!(parse_wait("1.5"), Ok(Duration::from_millis(1500)));
        assert_eq!(parse_wait("2s"), Ok(Duration::from_secs(2)));
        assert_eq!(parse_wait("250ms"), Ok(Duration::from_millis(250)));
        assert!(parse_wait("-1").is_err());
        assert!(parse_wait("soon").is_err());
    }

    #[test]
    fn bad_values_are_errors() {
        assert_eq!(
            ShotPlan::from_args(&args(&["n", "--size", "big", "--shot", "o.png"])),
            Err(ShotArgError::BadSize("big".to_owned()))
        );
        assert_eq!(
            ShotPlan::from_args(&args(&["n", "--shot"])),
            Err(ShotArgError::MissingValue("--shot"))
        );
        assert!(matches!(
            ShotPlan::from_args(&args(&["n", "--key", "<Nope>", "--shot", "o.png"])),
            Err(ShotArgError::BadKeys { .. })
        ));
    }

    #[test]
    fn steps_after_the_last_shot_are_an_error() {
        assert_eq!(
            ShotPlan::from_args(&args(&["n", "--shot", "o.png", "--wait", "1"])),
            Err(ShotArgError::StepsAfterLastShot)
        );
    }
}
