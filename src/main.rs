use std::fs::File;
use std::io::{Cursor, Read};
use std::process::Command;
use std::time::{Duration, Instant};

use image::ImageFormat;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::Camera;
use tiny_http::{Header, Response, Server, StatusCode};

// ---------- terminal styling: black discipline, cyan for the one thing that matters ----------

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";
const AMBER: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

// ANSI Shadow FIGlet face, pre-rendered. PASS in white, FOTO in cyan at print time.
const BANNER_PASS: [&str; 6] = [
    "██████╗  █████╗ ███████╗███████╗",
    "██╔══██╗██╔══██╗██╔════╝██╔════╝",
    "██████╔╝███████║███████╗███████╗",
    "██╔═══╝ ██╔══██║╚════██║╚════██║",
    "██║     ██║  ██║███████║███████║",
    "╚═╝     ╚═╝  ╚═╝╚══════╝╚══════╝",
];
const BANNER_FOTO: [&str; 6] = [
    "███████╗ ██████╗ ████████╗ ██████╗ ",
    "██╔════╝██╔═══██╗╚══██╔══╝██╔═══██╗",
    "█████╗  ██║   ██║   ██║   ██║   ██║",
    "██╔══╝  ██║   ██║   ██║   ██║   ██║",
    "██║     ╚██████╔╝   ██║   ╚██████╔╝",
    "╚═╝      ╚═════╝    ╚═╝    ╚═════╝ ",
];
const WORDMARK_W: usize = 70;

fn visible_len(line: &str) -> usize {
    // count only what's on screen: skip ANSI CSI (…m) and OSC (…BEL or …ESC\\) sequences
    let mut len = 0;
    let mut ch = line.chars();
    while let Some(c) = ch.next() {
        if c == '\x1b' {
            while let Some(d) = ch.next() {
                if d == 'm' || d == '\x07' || d == '\\' {
                    break;
                }
            }
        } else {
            len += 1;
        }
    }
    len
}

/// Terminal hyperlink (OSC 8); the URL itself stays visible as fallback text.
fn clickable(url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\")
}

fn framed(line: &str, width: usize) {
    let padding = " ".repeat(width.saturating_sub(visible_len(line)));
    println!("{DIM}│{RESET} {line}{padding}{RESET} {DIM}│{RESET}");
}

fn print_banner(port: u16, min_interval: u64, token: &str) {
    let mut hosts = vec![String::from("127.0.0.1")];
    for ip in lan_ips() {
        if !hosts.contains(&ip) {
            hosts.push(ip);
        }
    }
    let inner = WORDMARK_W;
    println!("{DIM}╭{}╮{RESET}", "─".repeat(inner));
    println!("{DIM}│{RESET}");
    for (pass_row, foto_row) in BANNER_PASS.iter().zip(BANNER_FOTO) {
        framed(&format!("{BOLD}{pass_row}{RESET} {BOLD}{CYAN}{foto_row}{RESET}"), inner);
    }
    println!("{DIM}│{RESET}");
    framed(
        &format!("{DIM}  v{} · webcam access for agents{RESET}", env!("CARGO_PKG_VERSION")),
        inner,
    );
    framed(&format!("{DIM}  localhost only · token dies when this process does{RESET}"), inner);
    println!("{DIM}│{RESET}");
    println!("{DIM}╰{}╯{RESET}", "─".repeat(inner));
    println!();
    println!("  {DIM}SESSION TOKEN{RESET}");
    println!("  {BOLD}{CYAN}{token}{RESET}");
    println!();
    println!("  {DIM}SNAPSHOT — give one URL to your agent · one JPEG per request{RESET}");
    for h in &hosts {
        println!("    {}", clickable(&format!("http://{h}:{port}/snapshot?token={token}")));
    }
    println!();
    println!("  {DIM}PREVIEW — click to open live view in your browser{RESET}");
    if let Some(lan) = hosts.get(1) {
        println!("    {BOLD}{CYAN}{}{RESET}", clickable(&format!("http://{lan}:{port}/preview?token={token}")));
    }
    println!("    {}", clickable(&format!("http://127.0.0.1:{port}/preview?token={token}")));
    println!();
    println!(
        "  {DIM}min interval {}s · reachable from your LAN · ctrl-c revokes everything{RESET}",
        min_interval
    );
    println!("{DIM}{}{RESET}", "─".repeat(inner + 4));
}

fn now_hms() -> String {
    // ponytail: UTC wall clock, no date dep — humans watching live can do the math
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 86_400;
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

fn log(outcome: &str, color: &str, route: &str, peer: &str, note: &str) {
    println!(
        "  {DIM}{}{RESET}  {color}{BOLD}{outcome:<9}{RESET} {route:<9} {peer:<15} {DIM}{note}{RESET}",
        now_hms()
    );
}

// ---------- token: from /dev/urandom, lives only in memory ----------

fn new_token() -> String {
    let mut f = File::open("/dev/urandom").expect("open /dev/urandom");
    let mut b = [0u8; 16];
    f.read_exact(&mut b).expect("read entropy");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---------- LAN discovery: macOS-native, no deps ----------

fn is_private(ip: &str) -> bool {
    if ip.starts_with("192.168.") || ip.starts_with("10.") {
        return true;
    }
    if let Some(rest) = ip.strip_prefix("172.") {
        return rest
            .split('.')
            .next()
            .and_then(|o| o.parse::<u8>().ok())
            .is_some_and(|o| (16..=31).contains(&o));
    }
    false
}

/// RFC1918 IPv4s of this Mac's active interfaces, via `ipconfig` (macOS ships it).
fn lan_ips() -> Vec<String> {
    let Ok(list) = Command::new("ipconfig").arg("getiflist").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&list.stdout)
        .split_whitespace()
        .filter(|i| *i != "lo0" && !i.starts_with("utun") && !i.starts_with("awdl"))
        .filter_map(|i| {
            let out = Command::new("ipconfig").arg("getifaddr").arg(i).output().ok()?;
            let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!ip.is_empty() && is_private(&ip)).then_some(ip)
        })
        .collect()
}

// ---------- pure helpers, unit-tested below ----------

fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    url.split_once('?')?
        .1
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// Ok(()) if a shot is allowed now, Err(wait) with the remaining cooldown otherwise.
fn throttle(last: Option<Instant>, min_interval: Duration, now: Instant) -> Result<(), Duration> {
    match last {
        None => Ok(()),
        Some(t) => match min_interval.checked_sub(now.duration_since(t)) {
            Some(wait) => Err(wait),
            None => Ok(()),
        },
    }
}

// ---------- camera ----------

struct Shooter(Camera);

impl Shooter {
    fn open() -> Self {
        println!("  {DIM}opening FaceTime camera … approve macOS permission if asked{RESET}");
        let mut cam = Camera::new(
            CameraIndex::Index(0),
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
        )
        .expect("no camera found");
        cam.open_stream().expect("cannot open camera stream");
        println!("  {DIM}camera ready{RESET}\n");
        Self(cam)
    }

    fn jpeg(&mut self) -> Vec<u8> {
        let frame = self.0.frame().expect("capture failed");
        let img = frame.decode_image::<RgbFormat>().expect("decode failed");
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Jpeg)
            .expect("jpeg encode failed");
        buf.into_inner()
    }
}

// ---------- http ----------

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut rest: &[String] = &argv;
    if let Some(first) = argv.first().map(String::as_str) {
        match first {
            "start" => rest = &argv[1..],
            "-h" | "--help" => {
                println!("usage: passfoto [start] [--port N] [--min-interval SECS]");
                return;
            }
            s if s.starts_with('-') => {} // bare flags, no subcommand
            other => {
                eprintln!("unknown command: {other}\nusage: passfoto [start] [--port N] [--min-interval SECS]");
                std::process::exit(2);
            }
        }
    }

    let mut port: u16 = 8741;
    let mut min_interval: u64 = 120;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--port" => port = iter.next().and_then(|v| v.parse().ok()).unwrap_or(port),
            "--min-interval" => {
                min_interval = iter.next().and_then(|v| v.parse().ok()).unwrap_or(min_interval)
            }
            other => eprintln!("unknown arg: {other}"),
        }
    }

    let server = Server::http(("0.0.0.0", port)).expect("cannot bind port");
    let token = new_token();
    print_banner(port, min_interval, &token);
    let mut shooter = Shooter::open();

    let interval = Duration::from_secs(min_interval);
    let mut last_shot: Option<Instant> = None;

    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or("").to_string();
        let peer = request
            .remote_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|| "?".into());

        // only the two real endpoints are gated; everything else (favicon, scanners)
        // gets a silent 404 — no camera, no token oracle, no log spam
        if path != "/snapshot" && path != "/preview" {
            respond(request, StatusCode(404), "text/plain", "not found\n");
            continue;
        }

        if query_param(&url, "token") != Some(token.as_str()) {
            log("DENY", RED, &path, &peer, "bad token");
            respond(request, StatusCode(401), "text/plain", "invalid or expired token\n");
            continue;
        }

        match path.as_str() {
            "/preview" => {
                log("PREVIEW", CYAN, "/preview", &peer, "browser session");
                respond(request, StatusCode(200), "text/html", &preview_html());
            }
            "/snapshot" => {
                // ?align= marks frames requested by the human tuning framing — skip the cooldown,
                // but they still count, so an agent polling mid-alignment stays throttled.
                let align = query_param(&url, "align").is_some();
                if !align {
                    if let Err(wait) = throttle(last_shot, interval, Instant::now()) {
                        log(
                            "THROTTLED",
                            AMBER,
                            "/snapshot",
                            &peer,
                            &format!("next in {}s", wait.as_secs() + 1),
                        );
                        let mut resp = Response::from_string(format!(
                            "throttled, retry in {}s\n",
                            wait.as_secs()
                        ))
                        .with_status_code(429);
                        if let Ok(h) =
                            Header::from_bytes("Retry-After", format!("{}", wait.as_secs() + 1))
                        {
                            resp = resp.with_header(h);
                        }
                        let _ = request.respond(resp);
                        continue;
                    }
                }

                let jpeg = shooter.jpeg();
                // align frames are the human's viewfinder — they neither consume nor
                // reset the agent cooldown, so framing in /preview can't starve an agent
                if !align {
                    last_shot = Some(Instant::now());
                }
                let outcome = if align { "ALIGN" } else { "GRANT" };
                log(outcome, CYAN, "/snapshot", &peer, &format!("{} kb", jpeg.len() / 1024));

                let header = Header::from_bytes("Content-Type", "image/jpeg").unwrap();
                let _ = request.respond(Response::from_data(jpeg).with_header(header));
            }
            _ => {} // unreachable: non-API paths are 404'd above
        }
    }
}

fn preview_html() -> String {
    format!(
        "<!doctype html><html><head><meta charset='utf-8'><title>passfoto · preview</title></head>\
         <body style='margin:0;background:#000;display:flex;flex-direction:column;align-items:center;justify-content:center;height:100vh;font-family:ui-monospace,Menlo,monospace'>\
         <img id='f' style='max-width:96vw;max-height:88vh;border:1px solid #222'>\
         <p style='color:#555;margin:12px'>passfoto live preview · refreshes every 2s</p>\
         <script>const q=location.search;const f=document.getElementById('f');\
         const tick=()=>{{f.src='/snapshot'+q+'&align='+Date.now()}};tick();setInterval(tick,2000);</script>\
         </body></html>")
}

fn respond(request: tiny_http::Request, status: StatusCode, content_type: &str, body: &str) {
    let header = Header::from_bytes("Content-Type", content_type).unwrap();
    let _ = request.respond(
        Response::from_string(body.to_string())
            .with_status_code(status)
            .with_header(header),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_32_hex_chars() {
        let t = new_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn query_param_finds_token_only() {
        assert_eq!(query_param("/snapshot?token=abc&x=1", "token"), Some("abc"));
        assert_eq!(query_param("/snapshot?token=abc&align=1", "align"), Some("1"));
        assert_eq!(query_param("/snapshot?x=1&token=abc", "token"), Some("abc"));
        assert_eq!(query_param("/snapshot", "token"), None);
        assert_eq!(query_param("/snapshot?not=abc", "token"), None);
    }

    #[test]
    fn throttle_blocks_within_window() {
        let now = Instant::now();
        let ivl = Duration::from_secs(120);
        assert!(throttle(None, ivl, now).is_ok());
        let last = now - Duration::from_secs(30);
        assert_eq!(throttle(Some(last), ivl, now).unwrap_err(), Duration::from_secs(90));
        let last = now - Duration::from_secs(121);
        assert!(throttle(Some(last), ivl, now).is_ok());
    }
}
