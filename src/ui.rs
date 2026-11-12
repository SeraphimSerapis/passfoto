//! Terminal output: banner, agent setup block and the live request log.

use std::net::IpAddr;
use std::process::Command;

// Black discipline, cyan for the one thing that matters.
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const CYAN: &str = "\x1b[36m";
pub const RED: &str = "\x1b[31m";
pub const AMBER: &str = "\x1b[33m";
pub const RESET: &str = "\x1b[0m";

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
            for d in ch.by_ref() {
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

pub struct BannerInfo<'a> {
    pub bind: IpAddr,
    pub port: u16,
    pub min_interval_s: u64,
    pub idle_timeout_s: u64,
    pub camera: u32,
    pub agent_token: &'a str,
    pub preview_token: &'a str,
}

pub fn print_banner(info: &BannerInfo) {
    let hosts = reachable_hosts(info.bind, &lan_ips());
    let base = |h: &str| format!("http://{h}:{}", info.port);
    let inner = WORDMARK_W;
    println!("{DIM}╭{}╮{RESET}", "─".repeat(inner));
    println!("{DIM}│{RESET}");
    for (pass_row, foto_row) in BANNER_PASS.iter().zip(BANNER_FOTO) {
        framed(
            &format!("{BOLD}{pass_row}{RESET} {BOLD}{CYAN}{foto_row}{RESET}"),
            inner,
        );
    }
    println!("{DIM}│{RESET}");
    framed(
        &format!(
            "{DIM}  v{} · webcam access for agents{RESET}",
            env!("CARGO_PKG_VERSION")
        ),
        inner,
    );
    let reach = if info.bind.is_unspecified() {
        "LAN + localhost"
    } else {
        "bound to one address"
    };
    framed(
        &format!("{DIM}  {reach} · tokens die when this process does{RESET}"),
        inner,
    );
    println!("{DIM}│{RESET}");
    println!("{DIM}╰{}╯{RESET}", "─".repeat(inner));
    println!();
    println!("  {DIM}AGENT SETUP · paste into the agent's shell{RESET}");
    // Prefer a LAN address: the agent may well run on another machine.
    let agent_host = hosts.get(1).unwrap_or(&hosts[0]);
    println!("    {BOLD}export PASSFOTO_URL={}{RESET}", base(agent_host));
    println!(
        "    {BOLD}export PASSFOTO_TOKEN={CYAN}{}{RESET}",
        info.agent_token
    );
    println!(
        "    {DIM}curl -H \"Authorization: Bearer $PASSFOTO_TOKEN\" -o shot.jpg \"$PASSFOTO_URL/snapshot\"{RESET}"
    );
    println!();
    println!("  {DIM}SNAPSHOT URLS · one JPEG per request{RESET}");
    for h in &hosts {
        println!(
            "    {}",
            clickable(&format!("{}/snapshot?token={}", base(h), info.agent_token))
        );
    }
    println!();
    println!("  {DIM}PREVIEW · for you, not the agent · separate token, no cooldown{RESET}");
    let preview = |h: &str| clickable(&format!("{}/preview?token={}", base(h), info.preview_token));
    for lan in &hosts[1..] {
        println!("    {BOLD}{CYAN}{}{RESET}", preview(lan));
    }
    println!("    {}", preview(&hosts[0]));
    println!();
    println!(
        "  {DIM}camera {} · min interval {}s · camera closes after {}s idle · ctrl-c revokes everything{RESET}",
        info.camera, info.min_interval_s, info.idle_timeout_s
    );
    println!("{DIM}{}{RESET}", "─".repeat(inner + 4));
}

/// Hosts to print URLs for, loopback first. Never empty.
fn reachable_hosts(bind: IpAddr, lan: &[String]) -> Vec<String> {
    if !bind.is_unspecified() {
        return vec![url_host(bind)];
    }
    let mut hosts = vec![String::from("127.0.0.1")];
    for ip in lan {
        if !hosts.contains(ip) {
            hosts.push(ip.clone());
        }
    }
    hosts
}

fn url_host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

fn now_hms() -> String {
    // UTC wall clock, no date dep; humans watching live can do the math
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

pub fn log(outcome: &str, color: &str, route: &str, peer: &str, note: &str) {
    println!(
        "  {DIM}{}{RESET}  {color}{BOLD}{outcome:<9}{RESET} {route:<15} {peer:<15} {DIM}{note}{RESET}",
        now_hms()
    );
}

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
            let out = Command::new("ipconfig")
                .arg("getifaddr")
                .arg(i)
                .output()
                .ok()?;
            let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!ip.is_empty() && is_private(&ip)).then_some(ip)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_private_gates_rfc1918() {
        assert!(is_private("192.168.1.2"));
        assert!(is_private("10.0.0.1"));
        assert!(is_private("172.16.0.1"));
        assert!(is_private("172.31.255.255"));
        assert!(!is_private("172.15.0.1"));
        assert!(!is_private("172.32.0.1"));
        assert!(!is_private("8.8.8.8"));
    }

    #[test]
    fn reachable_hosts_follow_bind() {
        let lan = vec!["192.168.1.5".to_string()];
        let any: IpAddr = "0.0.0.0".parse().unwrap();
        assert_eq!(reachable_hosts(any, &lan), ["127.0.0.1", "192.168.1.5"]);
        assert_eq!(reachable_hosts(any, &[]), ["127.0.0.1"]);
        assert_eq!(
            reachable_hosts("127.0.0.1".parse().unwrap(), &lan),
            ["127.0.0.1"]
        );
        assert_eq!(reachable_hosts("::1".parse().unwrap(), &lan), ["[::1]"]);
    }

    #[test]
    fn visible_len_skips_escapes() {
        assert_eq!(visible_len(&format!("{BOLD}ab{RESET}")), 2);
        assert_eq!(visible_len(&clickable("http://x")), 8);
    }
}
