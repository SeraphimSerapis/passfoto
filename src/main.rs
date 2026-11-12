mod api;
mod auth;
mod camera;
mod cli;
mod server;
mod throttle;
mod ui;

use std::io;
use std::net::SocketAddr;
use std::process::ExitCode;

use tiny_http::Server;

use crate::auth::Tokens;
use crate::camera::{CameraConfig, CameraHandle};
use crate::cli::{Command, Config};
use crate::throttle::Throttle;
use crate::ui::{BannerInfo, DIM, RESET};

/// Frames discarded after the camera opens so autoexposure settles before the real shot.
const WARMUP_FRAMES: u32 = 15;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        Ok(Command::Help) => {
            print!("{}", cli::usage());
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("passfoto {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::Cameras) => list_cameras(),
        Ok(Command::Start(cfg)) => start(cfg),
        Err(e) => {
            eprintln!("error: {e}\nrun `passfoto --help` for usage");
            ExitCode::from(2)
        }
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::FAILURE
}

fn list_cameras() -> ExitCode {
    match camera::list_cameras() {
        Ok(cams) if cams.is_empty() => fail("no cameras found"),
        Ok(cams) => {
            for c in cams {
                println!("{}\t{}", c.index, c.name);
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("cannot list cameras: {e}")),
    }
}

fn start(cfg: Config) -> ExitCode {
    let addr = SocketAddr::new(cfg.bind, cfg.port);
    let server = match Server::http(addr) {
        Ok(s) => s,
        Err(e) => {
            return fail(&bind_error(
                &cfg,
                e.downcast_ref::<io::Error>(),
                &e.to_string(),
            ))
        }
    };
    let tokens = match Tokens::generate() {
        Ok(t) => t,
        Err(e) => return fail(&format!("cannot read /dev/urandom for a token: {e}")),
    };

    ui::print_banner(&BannerInfo {
        bind: cfg.bind,
        port: cfg.port,
        min_interval_s: cfg.min_interval.as_secs(),
        idle_timeout_s: cfg.idle_timeout.as_secs(),
        camera: cfg.camera,
        agent_token: tokens.agent.as_str(),
        preview_token: tokens.preview.as_str(),
    });
    println!(
        "  {DIM}camera opens on the first capture · approve macOS permission if asked{RESET}\n"
    );

    let camera = CameraHandle::spawn(CameraConfig {
        index: cfg.camera,
        idle_timeout: cfg.idle_timeout,
        warmup_frames: WARMUP_FRAMES,
    });
    server::serve(
        server,
        server::App {
            tokens,
            throttle: Throttle::new(cfg.min_interval),
            camera,
        },
    );
    fail("http server stopped")
}

fn bind_error(cfg: &Config, io_err: Option<&io::Error>, fallback: &str) -> String {
    match io_err.map(io::Error::kind) {
        Some(io::ErrorKind::AddrInUse) => {
            format!("port {} is already in use, try --port <N>", cfg.port)
        }
        Some(io::ErrorKind::AddrNotAvailable) => {
            format!(
                "{} is not an address of this machine, check --bind",
                cfg.bind
            )
        }
        Some(io::ErrorKind::PermissionDenied) => {
            format!(
                "not allowed to listen on port {}, try a port above 1024",
                cfg.port
            )
        }
        _ => format!("cannot listen on {}:{}: {fallback}", cfg.bind, cfg.port),
    }
}
