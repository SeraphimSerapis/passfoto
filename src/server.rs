//! HTTP routing on a small pool of threads sharing one tiny_http server.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tiny_http::{Header, Request, Response, Server, StatusCode};

use crate::api::{self, CameraSummary, SnapshotParams, PREVIEW_MAX_WIDTH, PREVIEW_QUALITY};
use crate::auth::{presented_token, Role, Tokens};
use crate::camera::{CameraHandle, CameraState, CaptureError, CaptureOpts, Frame};
use crate::throttle::{ceil_secs, Throttle};
use crate::ui::{log, AMBER, CYAN, RED};

/// Enough that a slow client or a capture in progress never stalls `/status` or a 401.
const WORKERS: usize = 8;
/// How long an agent snapshot waits for an in-flight capture (usually a preview frame)
/// before giving up with 503.
const BUSY_WAIT: Duration = Duration::from_secs(5);
const BUSY_POLL: Duration = Duration::from_millis(100);

pub struct App {
    pub tokens: Tokens,
    pub throttle: Throttle,
    pub camera: CameraHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Snapshot,
    Status,
    Preview,
    PreviewFrame,
}

impl Route {
    fn from_path(path: &str) -> Option<Self> {
        match path {
            "/snapshot" => Some(Self::Snapshot),
            "/status" => Some(Self::Status),
            "/preview" => Some(Self::Preview),
            "/preview/frame" => Some(Self::PreviewFrame),
            _ => None,
        }
    }

    fn role(self) -> Role {
        match self {
            Self::Snapshot | Self::Status => Role::Agent,
            Self::Preview | Self::PreviewFrame => Role::Preview,
        }
    }
}

/// Serves until the server is dropped or every worker fails. Blocks the caller.
pub fn serve(server: Server, app: App) {
    let server = Arc::new(server);
    let app = Arc::new(app);
    let workers: Vec<_> = (0..WORKERS)
        .map(|i| {
            let server = Arc::clone(&server);
            let app = Arc::clone(&app);
            thread::Builder::new()
                .name(format!("passfoto-http-{i}"))
                .spawn(move || {
                    while let Ok(request) = server.recv() {
                        handle(&app, request);
                    }
                })
        })
        .collect();
    for w in workers.into_iter().flatten() {
        let _ = w.join();
    }
}

fn handle(app: &App, request: Request) {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("");
    let peer = request
        .remote_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "?".into());

    // Only real endpoints are gated; everything else (favicon, scanners) gets a
    // silent 404: no camera, no token oracle, no log spam.
    let Some(route) = Route::from_path(path) else {
        return send(request, text(404, "not found\n"));
    };

    let authorization = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str());
    let role = presented_token(authorization, &url).and_then(|t| app.tokens.role_of(t));
    if role != Some(route.role()) {
        let why = match role {
            None => "bad token",
            Some(Role::Agent) => "agent token on preview route",
            Some(Role::Preview) => "preview token on agent route",
        };
        log("DENY", RED, path, &peer, why);
        return send(request, text(401, "invalid or expired token\n"));
    }

    match route {
        Route::Status => {
            let state = app.camera.state();
            let body = api::status_json(
                app.throttle.min_interval(),
                app.throttle.status(Instant::now()),
                &camera_summary(&state),
            );
            log("STATUS", CYAN, path, &peer, "");
            send(
                request,
                with_header(
                    Response::from_string(body),
                    "Content-Type",
                    "application/json",
                ),
            );
        }
        Route::Preview => {
            log("PREVIEW", CYAN, path, &peer, "browser session");
            send(
                request,
                with_header(
                    Response::from_string(PREVIEW_HTML),
                    "Content-Type",
                    "text/html; charset=utf-8",
                ),
            );
        }
        Route::PreviewFrame => {
            // The human's viewfinder: no cooldown, never touches the agent's, downscaled
            // so the 2 s refresh stays cheap. Successes are not logged to keep the log readable.
            let opts = CaptureOpts {
                max_width: Some(PREVIEW_MAX_WIDTH),
                quality: PREVIEW_QUALITY,
            };
            match app.camera.capture(opts) {
                Ok(frame) => send(request, jpeg(frame)),
                Err(e) => {
                    if e != CaptureError::Busy {
                        log("ERROR", RED, path, &peer, &e.to_string());
                    }
                    send(request, capture_failed(&e));
                }
            }
        }
        Route::Snapshot => snapshot(app, request, &url, &peer),
    }
}

fn snapshot(app: &App, request: Request, url: &str, peer: &str) {
    let params = match api::parse_snapshot_params(url) {
        Ok(p) => p,
        Err(msg) => {
            log("BAD REQ", AMBER, "/snapshot", peer, &msg);
            return send(request, text(400, &format!("{msg}\n")));
        }
    };

    let reservation = match app.throttle.try_reserve(Instant::now()) {
        Ok(r) => r,
        Err(wait) => {
            let secs = ceil_secs(wait);
            log(
                "THROTTLED",
                AMBER,
                "/snapshot",
                peer,
                &format!("next in {secs}s"),
            );
            let resp = text(429, &format!("throttled, retry in {secs}s\n"));
            return send(request, with_header(resp, "Retry-After", &secs.to_string()));
        }
    };

    match capture_waiting(&app.camera, params) {
        Ok(frame) => {
            reservation.commit(Instant::now());
            let note = format!(
                "{}x{} {} kb",
                frame.width,
                frame.height,
                frame.jpeg.len() / 1024
            );
            log("GRANT", CYAN, "/snapshot", peer, &note);
            send(request, jpeg(frame));
        }
        Err(e) => {
            // Dropping the reservation leaves the cooldown as if this request never happened.
            drop(reservation);
            log("ERROR", RED, "/snapshot", peer, &e.to_string());
            send(request, capture_failed(&e));
        }
    }
}

/// An agent paid for this shot with its cooldown slot, so ride out a preview frame
/// in flight instead of failing straight away.
fn capture_waiting(camera: &CameraHandle, params: SnapshotParams) -> Result<Frame, CaptureError> {
    let opts = CaptureOpts {
        max_width: params.max_width,
        quality: params.quality,
    };
    let deadline = Instant::now() + BUSY_WAIT;
    loop {
        match camera.capture(opts) {
            Err(CaptureError::Busy) if Instant::now() < deadline => thread::sleep(BUSY_POLL),
            other => return other,
        }
    }
}

fn camera_summary(state: &CameraState) -> CameraSummary<'_> {
    match state {
        CameraState::Closed => CameraSummary {
            state: "closed",
            error: None,
        },
        CameraState::Opening => CameraSummary {
            state: "opening",
            error: None,
        },
        CameraState::Streaming => CameraSummary {
            state: "streaming",
            error: None,
        },
        CameraState::Failed { reason, .. } => CameraSummary {
            state: "failed",
            error: Some(reason),
        },
    }
}

type Resp = Response<std::io::Cursor<Vec<u8>>>;

fn capture_failed(e: &CaptureError) -> Resp {
    // Retry-After only where retrying can help; Unavailable needs the human.
    match e {
        CaptureError::Busy => {
            with_header(text(503, "camera busy, try again\n"), "Retry-After", "1")
        }
        CaptureError::Transient(_) => with_header(
            text(503, "camera capture failed, try again\n"),
            "Retry-After",
            "2",
        ),
        CaptureError::Unavailable(why) => text(503, &format!("camera unavailable: {why}\n")),
    }
}

fn jpeg(frame: Frame) -> Resp {
    let captured_at = api::rfc3339(frame.captured_at);
    let resp = with_header(
        Response::from_data(frame.jpeg),
        "Content-Type",
        "image/jpeg",
    );
    with_header(resp, "X-Captured-At", &captured_at)
}

fn text(status: u16, body: &str) -> Resp {
    with_header(
        Response::from_string(body).with_status_code(StatusCode(status)),
        "Content-Type",
        "text/plain; charset=utf-8",
    )
}

fn with_header(resp: Resp, name: &str, value: &str) -> Resp {
    match Header::from_bytes(name, value) {
        Ok(h) => resp.with_header(h),
        Err(()) => resp,
    }
}

fn send(request: Request, resp: Resp) {
    // Every response is a point-in-time view of the camera or the cooldown.
    let _ = request.respond(with_header(resp, "Cache-Control", "no-store"));
}

/// The page carries the preview token in its own URL and polls `/preview/frame` with it.
/// The next frame is requested only after the previous one settles, so a slow camera
/// cannot pile up requests.
const PREVIEW_HTML: &str = r"<!doctype html><html><head><meta charset='utf-8'><title>passfoto · preview</title></head>
<body style='margin:0;background:#000;display:flex;flex-direction:column;align-items:center;justify-content:center;height:100vh;font-family:ui-monospace,Menlo,monospace'>
<img id='f' alt='' style='max-width:96vw;max-height:88vh;border:1px solid #222'>
<p style='color:#555;margin:12px'>passfoto live preview · refreshes every 2s · does not use the agent's cooldown</p>
<script>
const q=location.search;let f=document.getElementById('f');
const next=()=>setTimeout(tick,2000);
const tick=()=>{const n=new Image();n.alt='';n.style.cssText=f.style.cssText;
n.onload=()=>{f.replaceWith(n);f=n;next()};n.onerror=next;
n.src='/preview/frame'+q+'&t='+Date.now()};
tick();
</script>
</body></html>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_require_the_matching_role() {
        assert_eq!(
            Route::from_path("/snapshot").map(Route::role),
            Some(Role::Agent)
        );
        assert_eq!(
            Route::from_path("/status").map(Route::role),
            Some(Role::Agent)
        );
        assert_eq!(
            Route::from_path("/preview").map(Route::role),
            Some(Role::Preview)
        );
        assert_eq!(
            Route::from_path("/preview/frame").map(Route::role),
            Some(Role::Preview)
        );
        assert_eq!(Route::from_path("/"), None);
        assert_eq!(Route::from_path("/snapshot/"), None);
        assert_eq!(Route::from_path("/favicon.ico"), None);
    }

    #[test]
    fn retry_after_only_when_retrying_helps() {
        let has_retry = |e: CaptureError| {
            capture_failed(&e)
                .headers()
                .iter()
                .any(|h| h.field.equiv("Retry-After"))
        };
        assert!(has_retry(CaptureError::Busy));
        assert!(has_retry(CaptureError::Transient("x".into())));
        assert!(!has_retry(CaptureError::Unavailable("x".into())));
    }
}
