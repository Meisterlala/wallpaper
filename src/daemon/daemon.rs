use std::convert::TryFrom;
use std::io::Read;
use std::mem::size_of;
use std::os::unix::net::*;
use std::path::PathBuf;
use std::process::exit;
use std::sync::{Arc, Mutex};
use std::thread::{self, sleep};
use std::time::Duration;
use std::fs;

use log::info;

mod state;
mod args;

use state::State;

//TODO: error handling
#[derive(Debug, Clone)]
pub enum Action {
    Random,
    Linear,
    Static(Option<PathBuf>),
}

fn main() {
    pretty_env_logger::init();

    let socket_path = "/tmp/test.socket";
    let time = Duration::new(60, 0);
    let data = Arc::new(Mutex::new(State::
                                   new(Action::Linear,
                                        time, format!("{}/Pictures/backgrounds/",
                                        std::env::var("HOME").unwrap()))));

    let listener = UnixListener::bind(socket_path).unwrap();
    let mut incoming = listener.incoming();

    info!("Binding socket {}", socket_path);

    let d = data.clone();
    thread::spawn(move || change_interval(d));

    while let Some(stream) = incoming.next() {
        let d = data.clone();
        let handle = thread::spawn(move || handle_connection(stream.unwrap(), d));
        if let Ok(res) = handle.join() {
            if res {
                break;
            }
        }
    }

    fs::remove_file(socket_path).expect("Can't delete socket");
    exit(0);
}

fn read_from_stream(mut stream: &UnixStream) -> String {
    use std::str;
    let mut string = String::new();

    //First run get length
    let mut buf: [u8; 8] = [0; 8];
    let mut buf_remaining: [u8; 1] = [0; 1];

    if let Err(_) = stream.read_exact(&mut buf) {
        return string
    }

    let length = usize::from_ne_bytes(buf);
    let buf_size = size_of::<usize>();

    for i in 0..=(length / buf_size) {
        if i >= length / buf_size {
            for _ in 0..(length % buf_size) {
                if let Ok(()) = stream.read_exact(&mut buf_remaining) {
                    if let Ok(str) = str::from_utf8(&buf_remaining) {
                        string.push_str(str);
                    }
                }
            }
        } else {
            if let Ok(()) = stream.read_exact(&mut buf) {
                if let Ok(str) = str::from_utf8(&buf) {
                    string.push_str(str);
                }
            }
        }
    }

    string
}

// Thread: Client <---> Server
fn handle_connection(mut stream: UnixStream, state: Arc<Mutex<State>>) -> bool {
    use std::io::prelude::*;
    info!("Handle new connection");
    let mut line = read_from_stream(&stream);
    let mut response = "".to_string();

    line = line.to_lowercase();
    info!("Got {}", &line);
    let message = args::Args::try_from(line.as_str());
    let mut stop_server = false;
    if let Ok(message) = message {
        use args::Args::*;
        use args::*;
        match message {
            Stop => stop_server = true,
            Next => state.lock().unwrap().next(),
            Prev => state.lock().unwrap().prev(),
            RNG => state.lock().unwrap().update_action(Action::Random),
            Linear => state.lock().unwrap().update_action(Action::Linear),
            Update => state.lock().unwrap().update_dir(),
            Save => state.lock().unwrap().save(),
            Shuffle => state.lock().unwrap().shuffle(),
            Hold(img) => {
                if let Some(img) = img {
                    state.lock().unwrap().update_action(Action::Static(Some(img.into())));
                } else {
                    state.lock().unwrap().update_action(Action::Static(None));
                }
            },
            Interval(d) => {
                state.lock().unwrap().change_interval(d);
            }
            Get(d) => {
                response = match d {
                    MessageArgs::Wallpaper => state.lock().unwrap().get_current_image().clone().to_str().unwrap_or("ERROR").to_owned(),
                    MessageArgs::Action => {
                        let action = state.lock().unwrap().get_action();
                        match action {
                            Action::Linear => "Linear".to_string(),
                            Action::Static(_) => "Static".to_string(),
                            Action::Random => "Random".to_string(),
                        }
                    },
                    MessageArgs::Duration => {
                        format!("{:?} seconds", state.lock().unwrap().get_change_interval())
                    }
                }
            },
        }
    } else {
        response = "I do not understand".to_string();
    }

    stream.write(&response.as_bytes()).unwrap();
    stop_server
}

fn change_interval(data: Arc<Mutex<State>>) {
    loop {
        let time = { //Go out of scope to unlock again
            let mut unlocked = data.lock().unwrap();
            unlocked.next();
            unlocked.get_change_interval()
        };
        sleep(time);
    }
}
