#![warn(missing_docs)]
use clap::clap_derive::ArgEnum;
use log::{error, info, trace, warn};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, fs, path::PathBuf, process::Command, time::Duration};

use crate::daemon::{Config, DaemonArgs};

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

    fn contains(&self, path: &PathBuf) -> bool {
        self.previous.contains(path)
    }
}

#[derive(Debug)]
pub struct WallpaperCommands {
    pub wallpaper_cmd: String,
    pub wallpaper_post_cmd: Option<String>,
    pub wallpaper_post_offset: Option<usize>,
}

impl WallpaperCommands {
    pub fn new(args: &DaemonArgs, config: &Config) -> Self {
        let wallpaper_cmd = args
            .wallpaper_change_command
            .as_ref()
            .unwrap_or(&config.wallpaper_change_command);
        let wallpaper_post_cmd = args
            .wallpaper_post_change_command
            .as_ref()
            .or(config.wallpaper_post_change_command.as_ref());
        let wallpaper_post_offset = args
            .wallpaper_post_change_offset
            .as_ref()
            .or(config.wallpaper_post_change_offset.as_ref());

        WallpaperCommands {
            wallpaper_cmd: wallpaper_cmd.to_owned(),
            wallpaper_post_cmd: wallpaper_post_cmd.cloned(),
            wallpaper_post_offset: wallpaper_post_offset.cloned(),
        }
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
    wallpaper_cmds: WallpaperCommands,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, ArgEnum, Serialize, Deserialize)]
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
        wallpaper_cmds: WallpaperCommands,
        history_max_size: usize,
    ) -> Self {
        let mut history = VecDeque::new();
        history.push_back(default_image.clone());

        let image_dir = if image_dir.starts_with("~/") {
            let mut home = PathBuf::from(std::env::var("HOME").unwrap());
            home.push(image_dir.components().skip(1).collect::<PathBuf>());
            home
        } else {
            image_dir
        };

        let default_image = if default_image.starts_with("~/") {
            let mut home = PathBuf::from(std::env::var("HOME").unwrap());
            home.push(default_image.components().skip(1).collect::<PathBuf>());
            home
        } else {
            default_image
        };

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
            wallpaper_cmds,
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
                    let num_pics = fs::read_dir(&self.image_dir).unwrap().count();

                    loop {
                        let idx = if self.action == NextImage::Random {
                            rand::thread_rng().gen_range(0..num_pics)
                        } else {
                            let mut idx = fs::read_dir(&self.image_dir)
                                .unwrap()
                                .filter_map(|res| res.ok().map(|e| e.path()))
                                .position(|elem| elem == *self.history.previous.back().unwrap())
                                .unwrap_or(0);
                            idx += 1;
                            idx %= num_pics;
                            idx
                        };

                        let wallpaper_path = fs::read_dir(&self.image_dir)
                            .unwrap()
                            .filter_map(|res| res.ok().map(|e| e.path()))
                            .nth(idx)
                            .unwrap();

                        if !self.history.contains(&wallpaper_path)
                            || num_pics <= self.history.history_max_size
                        {
                            self.history.push_back(wallpaper_path);
                            break;
                        }
                    }
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

        let wallpaper_cmd = self
            .wallpaper_cmds
            .wallpaper_cmd
            .replace("%wallpaper%", path.to_str().unwrap());

        trace!("Calling {:?}", wallpaper_cmd);
        let _process = Command::new("sh")
            .arg("-c")
            .arg(wallpaper_cmd)
            .output()
            .unwrap();

        if let Some(delay) = self.wallpaper_cmds.wallpaper_post_offset {
            if let Some(command) = &self.wallpaper_cmds.wallpaper_post_cmd {
                if let Some(prev) = self.history.previous.iter().rev().nth(delay) {
                    let prev = command.replace("%wallpaper%", prev.to_str().unwrap());
                    trace!("Calling {:?}", prev);
                    let _process = Command::new("sh").arg("-c").arg(&prev).output().unwrap();
                }
            }
        }
        Ok(())
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
