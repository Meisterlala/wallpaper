use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::net::*;
use std::os::unix::prelude::{FromRawFd, RawFd};
use std::path::PathBuf;
use std::process::exit;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread::{self, sleep};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use log::{debug, error, info};

mod state;

use state::*;

//TODO: error handling

/// Struct to hold and parse cli arguments
#[derive(Parser, Debug)]
#[clap(version)]
pub struct Cli {
    /// Image to show by default
    #[clap(short, long, value_parser, value_name = "FILE")]
    default: PathBuf,
    /// Socket for communication
    #[clap(short, long, value_parser, value_name = "FILE")]
    socket: Option<PathBuf>,
    /// Directory to search for images
    #[clap(short, long, value_parser, value_name = "DIRECTORY")]
    wallpaper_directory: PathBuf,
    /// Seach for images recursively
    #[clap(
        short,
        long,
        value_parser,
        value_name = "RECURSIVE",
        default_value_t = false
    )]
    recursivly: bool,
    /// Time in seconds between wallpaper changes
    #[clap(short, long, parse(try_from_str = parse_duration))]
    interval: Option<Duration>,
    /// File descriptor to write to to signal readiness
    #[clap(long)]
    fd: Option<RawFd>,
    /// Maximum size of the history (used for getting the previous wallpaper)
    #[clap(long, default_value_t = 25)]
    history_length: usize,
    #[clap(short, long, arg_enum, default_value_t = NextImage::Static)]
    mode: NextImage,
    /// Which underlying program to call to change the wallpaper
    #[clap(subcommand)]
    pub method: WallpaperMethod,
}

/// Program that gets called to change the active wallpaper
#[derive(Subcommand, Debug)]
pub enum WallpaperMethod {
    /// Use Feh (for xorg)
    Feh,
    /// Use hyprpaper (for Hyprland / wlroots based wayland compositors)
    Hyprpaper(HyprpaperOptions),
    Swww(SwwwOptions),
}

/// Hyprpaper needs a list of monitors. This struct holds them
#[derive(Args, Debug)]
pub struct HyprpaperOptions {
    #[clap(value_parser)]
    monitors: Vec<String>,
}

#[derive(Args, Debug)]
pub struct SwwwOptions {
    #[clap(long, default_value_t = 5)]
    duration: u8,
    #[clap(long, default_value_t = 120)]
    fps: u8,
    #[clap(long, default_value_t = 255)]
    step: u8,
}

fn parse_duration(arg: &str) -> Result<std::time::Duration, std::num::ParseIntError> {
    let seconds = arg.parse()?;
    Ok(std::time::Duration::from_secs(seconds))
}

fn main() {
    pretty_env_logger::init();

    let cli = Cli::parse();
    debug!("Command run was:\n{:?}", &cli);
    let socket = cli.socket.unwrap_or_else(|| {
        if let Ok(path) = std::env::var("XDG_RUNTIME_DIR") {
            let mut pathbuf = PathBuf::new();
            pathbuf.push(path);
            pathbuf.push("wallpaperd");
            pathbuf
        } else {
            PathBuf::from_str("/tmp/wallpaperd").unwrap()
        }
    });

    let s = socket.clone();
    ctrlc::set_handler(move || {
        if fs::remove_file(&s).is_err() {
            error!("Couldn't delete socket file");
        }
        exit(1);
    })
    .expect("Error setting signal hooks");

    let time = cli.interval.unwrap_or(Duration::new(60, 0));
    let data = Arc::new(Mutex::new(State::new(
        time,
        cli.wallpaper_directory,
        cli.default,
        cli.mode,
        cli.method,
        cli.history_length,
        cli.recursivly,
    )));

    info!("Binding socket {:?}", socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let incoming = listener.incoming();

    if cli.fd.is_some() {
        let mut file = unsafe { File::from_raw_fd(cli.fd.unwrap()) };
        writeln!(&mut file).unwrap();
    }

    let d = data.clone();
    thread::spawn(move || change_interval(d));

    for stream in incoming {
        let d = data.clone();
        let handle = thread::spawn(move || handle_connection(stream.unwrap(), d));
        if let Ok(res) = handle.join() {
            if res {
                break;
            }
        }
    }

    if fs::remove_file(&socket).is_err() {
        error!("Couldn't delete socket file");
        exit(1);
    }
    exit(0);
}

fn read_from_stream(mut stream: &UnixStream) -> String {
    let string = String::new();

    //First run get length
    let mut buf: [u8; 8] = [0; 8];

    if stream.read_exact(&mut buf).is_err() {
        return string;
    }

    let mut buffer = vec![0; usize::from_ne_bytes(buf)];
    if stream.read_exact(&mut buffer).is_err() {
        string
    } else {
        String::from_utf8(buffer).unwrap()
    }
}

#[derive(Parser)]
struct ClientMessage {
    #[clap(subcommand)]
    command: common::Command,
}

// Thread: Client <---> Server
fn handle_connection(mut stream: UnixStream, state: Arc<Mutex<State>>) -> bool {
    use common::*;
    use std::io::prelude::*;
    info!("Handle new connection");
    let line = read_from_stream(&stream);
    let mut response = "".to_string();

    debug!("Got {}", &line);
    let mut split: Vec<&str> = line.split(' ').collect();
    split.insert(0, " ");
    let mut stop_server = false;
    match ClientMessage::parse_from(split).command {
        Command::Next => state
            .lock()
            .unwrap()
            .change_image(ChangeImageDirection::Next),
        Command::Stop => stop_server = true,
        Command::Previous => state
            .lock()
            .unwrap()
            .change_image(ChangeImageDirection::Previous),
        Command::Mode(mode) => match mode {
            ModeArgs::Linear => state.lock().unwrap().update_action(NextImage::Linear, None),
            ModeArgs::Random => state.lock().unwrap().update_action(NextImage::Random, None),
            ModeArgs::Static(img) => match img.path {
                Some(path) => state
                    .lock()
                    .unwrap()
                    .update_action(NextImage::Static, Some(path)),
                None => state.lock().unwrap().update_action(NextImage::Static, None),
            },
        },
        Command::Fallback => state.lock().unwrap().save(),
        Command::Interval(d) => {
            state.lock().unwrap().change_interval(d.duration);
        }
        Command::Get(what) => {
            response = match what {
                GetArgs::Wallpaper => state
                    .lock()
                    .unwrap()
                    .get_current_image()
                    .clone()
                    .to_str()
                    .unwrap_or("ERROR")
                    .to_owned(),
                GetArgs::Duration => state
                    .lock()
                    .unwrap()
                    .get_change_interval()
                    .as_secs()
                    .to_string(),
                GetArgs::Mode => {
                    let action = state.lock().unwrap().get_action();
                    match action {
                        NextImage::Linear => "Linear".to_string(),
                        NextImage::Static => "Static".to_string(),
                        NextImage::Random => "Random".to_string(),
                    }
                }
                GetArgs::Fallback => state.lock().unwrap().get_fallback().to_string(),
            }
        }
    }

    stream.write_all(response.as_bytes()).unwrap();
    stop_server
}

fn change_interval(data: Arc<Mutex<State>>) {
    let mut time = {
        //Go out of scope to unlock again
        let unlocked = data.lock().unwrap();
        unlocked.get_change_interval()
    };
    loop {
        sleep(time);
        {
            //Go out of scope to unlock again
            let mut unlocked = data.lock().unwrap();
            unlocked.change_image(ChangeImageDirection::Next);
            time = unlocked.get_change_interval()
        };
    }
}
