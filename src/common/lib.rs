use std::{fmt::Display, path::PathBuf, time::Duration};

use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum Command {
    /// Show the next image
    Next,
    /// Exit the daemon
    Stop,
    /// Show the previous image
    Previous,
    /// Set the mode
    #[clap(subcommand)]
    Mode(ModeArgs),
    /// Display the fallback wallpaper
    /// If called again displays the previous image
    Fallback,
    /// Set the interval for new images in seconds
    Interval(IntervalDuration),
    /// Query information about the current state
    #[clap(subcommand)]
    Get(GetArgs),
}

#[derive(Args)]
pub struct IntervalDuration {
    #[clap(parse(try_from_str = parse_duration))]
    pub duration: Duration,
}

#[derive(Subcommand)]
pub enum ModeArgs {
    Linear,
    Random,
    Static(Image),
}

#[derive(Args)]
pub struct Image {
    pub path: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum GetArgs {
    Wallpaper,
    Duration,
    Mode,
    Fallback,
}

fn parse_duration(arg: &str) -> Result<std::time::Duration, std::num::ParseIntError> {
    let seconds = arg.parse()?;
    Ok(std::time::Duration::from_secs(seconds))
}

impl Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let args = match self {
            Command::Next => "next".to_string(),
            Command::Stop => "stop".to_string(),
            Command::Previous => "previous".to_string(),
            Command::Mode(mode) => match mode {
                ModeArgs::Linear => "mode linear".to_string(),
                ModeArgs::Random => "mode random".to_string(),
                ModeArgs::Static(img) => {
                    if let Some(path) = &img.path {
                        format!("mode static {}", path.to_string_lossy())
                    } else {
                        "mode static".to_string()
                    }
                }
            },
            Command::Fallback => "fallback".to_string(),
            Command::Interval(dur) => format!("interval {}", dur.duration.as_secs()),
            Command::Get(what) => match what {
                GetArgs::Wallpaper => "get wallpaper".to_string(),
                GetArgs::Duration => "get duration".to_string(),
                GetArgs::Mode => "get mode".to_string(),
                GetArgs::Fallback => "get fallback".to_string(),
            },
        };
        write!(f, "{args}")
    }
}
