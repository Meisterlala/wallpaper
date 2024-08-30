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

use clap::Parser;
use log::{debug, error, info};

use crate::state::*;
use serde::{Deserialize, Serialize};

//TODO: error handling

/// Struct to hold and parse cli arguments
#[derive(Parser, Debug, PartialEq, Eq)]
#[clap(version)]
pub struct DaemonArgs {
    #[clap(short, long, value_parser, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Image to show by default
    #[clap(short, long, value_parser, value_name = "FILE")]
    default: Option<PathBuf>,
    /// Socket for communication
    #[clap(short, long, value_parser, value_name = "FILE")]
    socket: Option<PathBuf>,
    /// Directory to search for images
    #[clap(short, long, value_parser, value_name = "DIRECTORY")]
    wallpaper_directory: Option<PathBuf>,
    /// Time in seconds between wallpaper changes
    #[clap(short, long, parse(try_from_str = parse_duration))]
    interval: Option<Duration>,
    /// File descriptor to write to to signal readiness
    #[clap(long)]
    fd: Option<RawFd>,
    /// Maximum size of the history (used for getting the previous wallpaper)
    #[clap(long)]
    history_length: Option<usize>,
    #[clap(short, long, arg_enum)]
    mode: Option<NextImage>,
    /// Command to call to change the wallpaper
    /// calls 'sh -c ${wallpaper_change_command}'
    /// %wallpaper% gets replaced with the path to the wallpaper
    #[clap(long)]
    wallpaper_change_command: Option<String>,
    /// Command to call after changing the wallpaper
    /// calls 'sh -c ${wallpaper_post_change_command}'
    /// %wallpaper% gets replaced with the path to the wallpaper
    #[clap(long)]
    wallpaper_post_change_command: Option<String>,
    /// How many cycles of delay to keep
    #[clap(long)]
    wallpaper_post_change_offset: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct Config {
    /// Image to show by default
    default_image: PathBuf,
    /// Directory to search for images
    wallpaper_directory: PathBuf,
    /// Time in seconds between wallpaper changes
    interval: u64,
    /// Maximum size of the history (used for getting the previous wallpaper)
    history_length: usize,
    mode: NextImage,
    /// Command to call to change the wallpaper
    /// calls 'sh -c ${wallpaper_change_command}'
    /// %wallpaper% gets replaced with the path to the wallpaper
    wallpaper_change_command: String,
    /// Command to call after changing the wallpaper
    /// calls 'sh -c ${wallpaper_post_change_command}'
    /// %wallpaper% gets replaced with the path to the wallpaper
    wallpaper_post_change_command: Option<String>,
    /// How many cycles of delay to keep
    wallpaper_post_change_offset: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_image: PathBuf::from_str("~/Pictures/wallpaper.png").unwrap(),
            wallpaper_directory: PathBuf::from_str("~/Pictures/wallpapers/").unwrap(),
            interval: 60,
            history_length: 25,
            mode: NextImage::Random,
            wallpaper_change_command: "feh -r %wallpaper%".to_owned(),
            wallpaper_post_change_command: None,
            wallpaper_post_change_offset: None,
        }
    }
}

fn parse_duration(arg: &str) -> Result<std::time::Duration, std::num::ParseIntError> {
    let seconds = arg.parse()?;
    Ok(std::time::Duration::from_secs(seconds))
}

pub fn start_daemon(args: DaemonArgs) {
    debug!("Command run was:\n{:?}", &args);

    let config_file = args.config.unwrap_or_else(|| {
        let mut dotconfig = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_arg| {
                let mut home = PathBuf::from(std::env::var("HOME").unwrap());
                home.push(".config");
                home
            });

        dotconfig.push("wallpaperd");
        dotconfig.push("wallpaperd.toml");
        dotconfig
    });

    let config = if config_file.is_file() {
        // Parse config with serde
        let config = fs::read_to_string(config_file).expect("Couldn't read config file");
        let config: Config = toml::from_str(&config).expect("Invalid config");
        config
    } else {
        Config::default()
    };

    let socket = args.socket.unwrap_or_else(|| {
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

    let time = args
        .interval
        .unwrap_or(Duration::from_secs(config.interval));

    let wallpaper_cmds = WallpaperCommands {
        wallpaper_cmd: args
            .wallpaper_change_command
            .unwrap_or(config.wallpaper_change_command),
        wallpaper_post_cmd: args
            .wallpaper_post_change_command
            .or(config.wallpaper_post_change_command),
        wallpaper_post_offset: args
            .wallpaper_post_change_offset
            .or(config.wallpaper_post_change_offset),
    };

    let data = Arc::new(Mutex::new(State::new(
        time,
        args.wallpaper_directory
            .unwrap_or(config.wallpaper_directory),
        args.default.unwrap_or(config.default_image),
        args.mode.unwrap_or(config.mode),
        wallpaper_cmds,
        args.history_length.unwrap_or(config.history_length),
    )));

    info!("Binding socket {:?}", socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let incoming = listener.incoming();

    if args.fd.is_some() {
        let mut file = unsafe { File::from_raw_fd(args.fd.unwrap()) };
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
    command: crate::command::Command,
}

// Thread: Client <---> Server
fn handle_connection(mut stream: UnixStream, state: Arc<Mutex<State>>) -> bool {
    use crate::command::*;
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
        Command::Daemon(_) => todo!(),
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
