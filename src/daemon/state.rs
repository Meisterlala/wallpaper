#![warn(missing_docs)]
use clap::clap_derive::ArgEnum;
use log::{error, info, trace, warn};
use rand::Rng;
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::Command,
    time::Duration,
};

use crate::WallpaperMethod;

#[derive(Debug)]
struct History {
    previous: VecDeque<PathBuf>, // Never empty
    next: Vec<PathBuf>,          // Possibly empty
    history_max_size: usize,
}

impl History {
    fn has_next(&self) -> bool {
        !self.next.is_empty()
    }

    fn has_previous(&self) -> bool {
        // previous must not be empty
        self.previous.len() >= 2
    }

    fn go_next(&mut self) {
        if self.has_next() {
            let image = self.next.pop().unwrap();
            self.push_back(image);
        } else {
            warn!("Calling go_next without next image existing");
        }
    }

    fn go_previous(&mut self) {
        if self.has_previous() {
            self.next.push(self.previous.pop_back().unwrap());
        } else {
            warn!("Calling go_previous without previous image existing");
        }
    }

    fn push_back(&mut self, path: PathBuf) {
        if self.previous.len() >= self.history_max_size {
            self.previous.pop_front();
        }
        self.previous.push_back(path);
    }
}

/// Global object to store the current state
#[derive(Debug)]
pub struct State {
    history: History,
    action: NextImage,
    previous_action: NextImage,
    change_interval: Duration,
    image_dir: PathBuf,
    use_fallback: bool,
    default_image: PathBuf,
    wallpaper_cmd: WallpaperMethod,
    recusive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, ArgEnum)]
pub enum NextImage {
    Random,
    Linear,
    Static,
}

pub enum ChangeImageDirection {
    Next,
    Previous,
}

impl State {
    pub fn new(
        change_interval: Duration,
        image_dir: PathBuf,
        default_image: PathBuf,
        action: NextImage,
        wallpaper_cmd: WallpaperMethod,
        history_max_size: usize,
        recusive: bool,
    ) -> Self {
        let mut history = VecDeque::new();
        history.push_back(default_image.clone());

        State {
            history: History {
                previous: history,
                next: Vec::new(),
                history_max_size,
            },
            action,
            previous_action: action,
            change_interval,
            image_dir,
            use_fallback: false,
            default_image,
            wallpaper_cmd,
            recusive,
        }
    }

    pub fn change_image(&mut self, direction: ChangeImageDirection) {
        if self.use_fallback {
            info!("Can't change image while using fallback");
            return;
        }
        if let NextImage::Static = self.action {
            info!("Can't change image while in static mode");
            return;
        }

        match direction {
            ChangeImageDirection::Next => {
                info!("Going to the next image");
                // "Redo"
                if self.history.has_next() {
                    self.history.go_next();
                } else {
                    // If not enough space delete one element
                    let mut idx = fs::read_dir(&self.image_dir)
                        .unwrap()
                        .filter_map(|res| res.ok().map(|e| e.path()))
                        .position(|elem| elem == *self.history.previous.back().unwrap())
                        .unwrap_or(0);

                    let num_pics = fs::read_dir(&self.image_dir)
                        .unwrap()
                        .filter_map(|res| res.ok().map(|e| e.path()))
                        .count();

                    if self.action == NextImage::Random {
                        idx = rand::thread_rng().gen_range(0..num_pics);
                    } else {
                        idx += 1;
                        idx %= num_pics;
                    }

                    self.history.push_back(
                        fs::read_dir(&self.image_dir)
                            .unwrap()
                            .filter_map(|res| res.ok().map(|e| e.path()))
                            .nth(idx)
                            .unwrap(),
                    );
                }
            }
            ChangeImageDirection::Previous => {
                info!("Going to the previous image");
                if self.history.has_previous() {
                    self.history.go_previous();
                } else {
                    info!("There is no previous image");
                }
            }
        }

        // Update current image
        if self.update().is_err() {
            error!("Error setting the wallpaper");
        }
    }

    pub fn update(&self) -> Result<(), ()> {
        info!("Updating current wallpaper");
        let path = self.get_current_image();
        trace!("setting wallpaper to {}", path.to_string_lossy());
        match &self.wallpaper_cmd {
            WallpaperMethod::Feh => {
                let mut process = if self.recusive {
                    Command::new("feh")
                        .arg("-r")
                        .arg("--bg-fill")
                        .arg(path)
                        .spawn()
                        .unwrap()
                } else {
                    Command::new("feh")
                        .arg("--bg-fill")
                        .arg(path)
                        .spawn()
                        .unwrap()
                };
                process.wait().unwrap();
            }
            WallpaperMethod::Hyprpaper(args) => {
                self.update_hyprpaper(args)?;
            }
            WallpaperMethod::Swww(args) => {
                let angle = rand::thread_rng().gen_range(0..360);
                let mut process = Command::new("swww")
                    .arg("img")
                    .arg("--transition-fps")
                    .arg(format!("{}", args.fps))
                    .arg("--transition-step")
                    .arg(format!("{}", args.step))
                    .arg("--transition-duration")
                    .arg(format!("{}", args.duration))
                    .arg("--transition-type")
                    .arg("wipe")
                    .arg("--transition-angle")
                    .arg(format!("{}", angle))
                    .arg(path)
                    .spawn()
                    .unwrap();
                process.wait().unwrap();
            }
        }
        Ok(())
    }

    fn update_hyprpaper(&self, args: &crate::HyprpaperOptions) -> Result<(), ()>  {
        // Preload the wallpaper
        self.send_to_hyprpaper(
            format!("preload {}", self.get_current_image().to_string_lossy()).as_bytes(),
        )?;
        // Display the wallpaper on every monitor
        for monitor in args.monitors.iter() {
            self.send_to_hyprpaper(
                format!(
                    "wallpaper {monitor},{}",
                    self.get_current_image().to_string_lossy()
                )
                .as_bytes(),
            )?;
        }

        if self.history.previous.len() > 2 {
            let prev = self.history.previous.iter().rev().nth(2).unwrap();
            if prev != self.get_current_image() {
                self.send_to_hyprpaper(
                    format!("unload {}", prev.to_string_lossy()).as_bytes(),
                )?;
            }
        }
        Ok(())
    }

    fn send_to_hyprpaper(&self, msg: &[u8]) -> Result<String, ()> {
        let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").map_err(|_| ())?;
        let path: PathBuf = ["/tmp/hypr", &signature, ".hyprpaper.sock"]
            .iter()
            .collect();

        info!("Connecting to socket at {}", path.to_string_lossy());

        let mut listener = UnixStream::connect(path).map_err(|_| ())?;

        listener.write_all(msg).map_err(|_| ())?;
        listener.flush().map_err(|_| ())?;
        info!("Written: {:?}", msg);
        let mut buffer = String::new();

        info!("Created buffer");
        listener.read_to_string(&mut buffer).map_err(|_| ())?;
        info!("Got result: {buffer}");

        listener.write_all(b"").map_err(|_| ())?;
        listener.flush().map_err(|_| ())?;

        Ok(buffer)
    }

    pub fn update_action(&mut self, action: NextImage, image: Option<PathBuf>) {
        info!("Setting action to {:?}", action);
        self.action = action;
        if let Some(image) = image {
            self.history.push_back(image);
            if self.update().is_err() {
                error!("Error setting the wallpaper");
            }
        }
    }

    pub fn save(&mut self) {
        self.use_fallback = !self.use_fallback;
        info!("Setting fallback to {}", self.use_fallback);
        if self.use_fallback {
            self.previous_action = self.action;
            self.action = NextImage::Static;
            self.history.push_back(self.default_image.clone());
        } else {
            self.action = self.previous_action;
            self.history.previous.pop_back();
        }
        if self.update().is_err() {
            error!("Error setting the wallpaper");
        }
    }

    pub fn get_current_image(&self) -> &PathBuf {
        self.history.previous.back().unwrap()
    }

    pub fn get_action(&self) -> NextImage {
        self.action
    }

    pub fn change_interval(&mut self, i: Duration) {
        self.change_interval = i;
    }

    pub fn get_change_interval(&self) -> Duration {
        self.change_interval
    }

    pub fn get_fallback(&self) -> bool {
        self.use_fallback
    }
}
