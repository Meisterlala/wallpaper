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
    #[clap(short, long, parse(try_from_str = parse_duration), default_value = "60")]
    interval: Duration,
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
    pub wallpaper_change_command: Option<String>,
    /// Command to call after changing the wallpaper
    /// calls 'sh -c ${wallpaper_post_change_command}'
    /// %wallpaper% gets replaced with the path to the wallpaper
    #[clap(long)]
    pub wallpaper_post_change_command: Option<String>,
    /// How many cycles of delay to keep
    #[clap(long)]
    pub wallpaper_post_change_offset: Option<usize>,
}

pub struct Config {
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
    pub wallpaper_change_command: String,
    /// Command to call after changing the wallpaper
    /// calls 'sh -c ${wallpaper_post_change_command}'
    /// %wallpaper% gets replaced with the path to the wallpaper
    pub wallpaper_post_change_command: Option<String>,
    /// How many cycles of delay to keep
    pub wallpaper_post_change_offset: Option<usize>,
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

impl FromStr for Config {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut config = Config::default();
        for line in s.lines() {
            let split: Vec<&str> = line.split("=").collect();
            let left = split[0].trim();
            let right = split[1]
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim();
            match left {
                "default_image" => config.default_image = right.into(),
                "wallpaper_directory" => config.wallpaper_directory = right.into(),
                "interval" => {
                    if let Ok(seconds) = u64::from_str(right) {
                        config.interval = seconds;
                    } else {
                        error!("Couldn't parse interval in config file");
                    }
                }
                "history_length" => {
                    if let Ok(length) = usize::from_str(right) {
                        config.history_length = length;
                    } else {
                        error!("Couldn't parse history_length in config file");
                    }
                }
                "mode" => {
                    config.mode = match right {
                        "Random" => NextImage::Random,
                        "Linear" => NextImage::Linear,
                        "Static" => NextImage::Static,
                        _ => {
                            error!("Unknown wallpaper mode: {right}");
                            config.mode
                        }
                    }
                }
                "wallpaper_change_command" => config.wallpaper_change_command = right.into(),
                "wallpaper_post_change_command" => {
                    config.wallpaper_post_change_command = Some(right.into())
                }
                "wallpaper_post_change_offset" => {
                    if let Ok(offset) = usize::from_str(right) {
                        config.wallpaper_post_change_offset = Some(offset);
                    } else {
                        error!("Couldn't parse wallpaper_post_change_offset");
                    }
                }
                _ => {
                    error!("{left} isn't a valid key");
                }
            }
        }
        Ok(config)
    }
}

fn parse_duration(arg: &str) -> Result<std::time::Duration, std::num::ParseIntError> {
    let seconds = arg.parse()?;
    Ok(std::time::Duration::from_secs(seconds))
}

#[derive(Debug)]
struct UnixSocketWithDrop {
    path: PathBuf,
    socket: UnixListener,
}

impl Drop for UnixSocketWithDrop {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).unwrap();
        info!("Removing socket file");
    }
}

pub fn start_daemon(args: DaemonArgs) {
    let config_file = get_config_file(&args);

    let config = if config_file.is_file() {
        let config = fs::read_to_string(config_file).expect("Couldn't read config file");
        // Can't fail
        Config::from_str(&config).unwrap()
    } else {
        Config::default()
    };

    let wallpaper_cmds = WallpaperCommands::new(&args, &config);

    let socket = get_socket(&args);
    info!("Binding socket {:?}", socket);

    let s = socket.path.clone();
    ctrlc::set_handler(move || {
        if fs::remove_file(&s).is_err() {
            error!("Couldn't delete socket file");
        }
        exit(0);
    })
    .expect("Error setting signal hooks");

    let incoming = socket.socket.incoming();

    let data = Arc::new(Mutex::new(State::new(
        args.interval,
        args.wallpaper_directory
            .unwrap_or(config.wallpaper_directory),
        args.default.unwrap_or(config.default_image),
        args.mode.unwrap_or(config.mode),
        wallpaper_cmds,
        args.history_length.unwrap_or(config.history_length),
    )));

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
}

fn get_config_file(args: &DaemonArgs) -> PathBuf {
    args.config.as_ref().map_or_else(
        || {
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
        },
        |val| val.to_owned(),
    )
}

fn get_socket(args: &DaemonArgs) -> UnixSocketWithDrop {
    let path = args.socket.as_ref().map_or_else(
        || {
            if let Ok(path) = std::env::var("XDG_RUNTIME_DIR") {
                let mut pathbuf = PathBuf::new();
                pathbuf.push(path);
                pathbuf.push("wallpaperd");
                pathbuf
            } else {
                PathBuf::from_str("/tmp/wallpaperd").unwrap()
            }
        },
        |val| val.to_owned(),
    );

    let socket = UnixListener::bind(&path).unwrap();

    UnixSocketWithDrop { path, socket }
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
                GetArgs::WpDir => state.lock().unwrap().get_image_dir().to_str().unwrap().to_owned(),
            }
        }
        Command::Daemon(_) => todo!(),
        Command::WpDir(wallpaper_directory) => {
            state.lock().unwrap().set_image_dir(wallpaper_directory.path);
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
