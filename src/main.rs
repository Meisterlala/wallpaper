use clap::Parser;
use command::Command;
use std::io::prelude::*;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::str::FromStr;

use log::info;

mod command;
mod daemon;
mod state;

#[derive(Parser)]
#[clap(version)]
pub struct Arguments {
    /// Socket for communication
    #[clap(short, long, value_parser, value_name = "FILE")]
    socket: Option<PathBuf>,
    #[clap(subcommand)]
    command: command::Command,
}

fn main() {
    pretty_env_logger::init();

    let args = Arguments::parse();
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

    //TODO make not shit

    if let Command::Daemon(args) = args.command {
        daemon::start_daemon(args);
    } else {
        let args = args.command.to_string();
        let len = args.len();
        let mut socket = UnixStream::connect(socket).expect("Socket not found");

        info!("Sending {:?}", args);
        socket.write_all(&len.to_ne_bytes()).unwrap();
        socket.write_all(args.trim().as_bytes()).unwrap();
        info!("{:?}", &len.to_ne_bytes());
        info!("{:?}", args.trim().as_bytes());
        socket.flush().unwrap();

        info!("Reading:");
        let mut line = String::new();
        socket
            .read_to_string(&mut line)
            .expect("Couldn't read string");
        println!("{}", line);
    }
}
