use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    pub min_interval: Duration,
    pub camera: u32,
    pub idle_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // All interfaces by default so an agent on another LAN machine can reach it.
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 8741,
            min_interval: Duration::from_secs(120),
            camera: 0,
            idle_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start(Config),
    Cameras,
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    UnknownCommand(String),
    UnknownFlag(String),
    MissingValue(&'static str),
    BadValue {
        flag: &'static str,
        value: String,
        expected: &'static str,
    },
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(c) => write!(f, "unknown command: {c}"),
            Self::UnknownFlag(a) => write!(f, "unknown flag: {a}"),
            Self::MissingValue(flag) => write!(f, "{flag} needs a value"),
            Self::BadValue {
                flag,
                value,
                expected,
            } => {
                write!(f, "{flag}: '{value}' is not {expected}")
            }
        }
    }
}

pub fn usage() -> String {
    let d = Config::default();
    format!(
        "passfoto {version}
Ephemeral-token webcam API for agents on macOS.

USAGE:
    passfoto [start] [OPTIONS]    serve the webcam over HTTP (default)
    passfoto cameras              list camera devices and their indexes
    passfoto help | --help | -h   show this help
    passfoto version | --version | -V

OPTIONS (start):
    --port <N>             HTTP port [default: {port}]
    --bind <ADDR>          IP address to listen on; 127.0.0.1 keeps it local [default: {bind}]
    --min-interval <SECS>  cooldown between agent snapshots [default: {min}]
    --camera <INDEX>       camera device index, see `passfoto cameras` [default: {cam}]
    --idle-timeout <SECS>  close the camera after this long without a capture [default: {idle}]
",
        version = env!("CARGO_PKG_VERSION"),
        port = d.port,
        bind = d.bind,
        min = d.min_interval.as_secs(),
        cam = d.camera,
        idle = d.idle_timeout.as_secs(),
    )
}

pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Command, CliError> {
    let mut args = args.iter().map(AsRef::as_ref).peekable();
    match args.peek().copied() {
        Some("start") => {
            args.next();
        }
        Some("cameras") => {
            args.next();
            return match args.next() {
                None => Ok(Command::Cameras),
                Some(extra) => Err(CliError::UnknownFlag(extra.to_string())),
            };
        }
        Some("help") => return Ok(Command::Help),
        Some("version") => return Ok(Command::Version),
        Some(s) if !s.starts_with('-') => return Err(CliError::UnknownCommand(s.to_string())),
        _ => {}
    }

    let mut cfg = Config::default();
    while let Some(arg) = args.next() {
        // Accept both `--port 9000` and `--port=9000`.
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (arg, None),
        };
        match flag {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--port" => cfg.port = value(&mut args, inline, "--port", "a port number (1-65535)")?,
            "--bind" => cfg.bind = value(&mut args, inline, "--bind", "an IP address")?,
            "--min-interval" => {
                cfg.min_interval =
                    secs(value(&mut args, inline, "--min-interval", "whole seconds")?)
            }
            "--camera" => cfg.camera = value(&mut args, inline, "--camera", "a camera index")?,
            "--idle-timeout" => {
                cfg.idle_timeout =
                    secs(value(&mut args, inline, "--idle-timeout", "whole seconds")?)
            }
            other => return Err(CliError::UnknownFlag(other.to_string())),
        }
    }
    if cfg.port == 0 {
        return Err(CliError::BadValue {
            flag: "--port",
            value: "0".into(),
            expected: "a port number (1-65535)",
        });
    }
    Ok(Command::Start(cfg))
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn value<'a, T: std::str::FromStr>(
    args: &mut impl Iterator<Item = &'a str>,
    inline: Option<&'a str>,
    flag: &'static str,
    expected: &'static str,
) -> Result<T, CliError> {
    let raw = match inline {
        Some(v) => v,
        None => args.next().ok_or(CliError::MissingValue(flag))?,
    };
    raw.parse().map_err(|_| CliError::BadValue {
        flag,
        value: raw.to_string(),
        expected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(args: &[&str]) -> Config {
        match parse(args) {
            Ok(Command::Start(c)) => c,
            other => panic!("expected Start, got {other:?}"),
        }
    }

    #[test]
    fn no_args_and_bare_start_use_defaults() {
        assert_eq!(start(&[]), Config::default());
        assert_eq!(start(&["start"]), Config::default());
    }

    #[test]
    fn all_flags_parse() {
        let c = start(&[
            "start",
            "--port",
            "9000",
            "--bind",
            "127.0.0.1",
            "--min-interval",
            "5",
            "--camera",
            "2",
            "--idle-timeout=10",
        ]);
        assert_eq!(c.port, 9000);
        assert_eq!(c.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(c.min_interval, Duration::from_secs(5));
        assert_eq!(c.camera, 2);
        assert_eq!(c.idle_timeout, Duration::from_secs(10));
    }

    #[test]
    fn flags_work_without_start() {
        assert_eq!(start(&["--port=1234"]).port, 1234);
        assert_eq!(
            start(&["--bind", "::1"]).bind,
            "::1".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn subcommands_and_meta_flags() {
        assert_eq!(parse(&["cameras"]), Ok(Command::Cameras));
        assert_eq!(parse(&["help"]), Ok(Command::Help));
        assert_eq!(parse(&["version"]), Ok(Command::Version));
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(
            parse(&["start", "--port", "1", "--help"]),
            Ok(Command::Help)
        );
        assert_eq!(parse(&["-V"]), Ok(Command::Version));
        assert_eq!(parse(&["--version"]), Ok(Command::Version));
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        assert_eq!(
            parse(&["snap"]),
            Err(CliError::UnknownCommand("snap".into()))
        );
        assert_eq!(
            parse(&["--nope"]),
            Err(CliError::UnknownFlag("--nope".into()))
        );
        assert_eq!(parse(&["--port"]), Err(CliError::MissingValue("--port")));
        assert!(matches!(
            parse(&["--port", "99999"]),
            Err(CliError::BadValue { flag: "--port", .. })
        ));
        assert!(matches!(
            parse(&["--port", "0"]),
            Err(CliError::BadValue { flag: "--port", .. })
        ));
        assert!(matches!(
            parse(&["--bind", "localhost"]),
            Err(CliError::BadValue { flag: "--bind", .. })
        ));
        assert!(matches!(
            parse(&["--min-interval", "-1"]),
            Err(CliError::BadValue { .. })
        ));
        assert_eq!(
            parse(&["cameras", "--port"]),
            Err(CliError::UnknownFlag("--port".into()))
        );
    }

    #[test]
    fn usage_lists_every_flag() {
        let u = usage();
        for flag in [
            "--port",
            "--bind",
            "--min-interval",
            "--camera",
            "--idle-timeout",
            "--help",
            "--version",
            "cameras",
        ] {
            assert!(u.contains(flag), "usage is missing {flag}");
        }
    }
}
