//! Webcam access behind a dedicated thread.
//!
//! The nokhwa `Camera` never leaves its worker thread; callers hold a cheap,
//! cloneable `CameraHandle` and talk to that thread over channels. The worker
//! opens the device lazily, keeps it warm while captures keep coming, closes it
//! after `idle_timeout`, and reopens it with backoff after a failure.

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::{Buffer, Camera, NokhwaError};

/// Upper bound for one capture as seen by the caller, including open and warmup.
/// nokhwa's AVFoundation `frame()` blocks forever once a device stops delivering
/// frames (e.g. unplugged), so this is the only way a dead camera is noticed.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// While streaming idle, frames are pulled this often and discarded. nokhwa queues
/// every frame in an unbounded channel and `frame()` returns the oldest one, so
/// without pumping the queue grows without limit and captures come back stale.
const PUMP_INTERVAL: Duration = Duration::from_millis(250);

const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Retry hint for `Busy` and for decode/encode errors, which have no backoff.
const BUSY_RETRY: Duration = Duration::from_secs(1);

const PERMISSION_HINT: &str = "camera permission not granted; approve the macOS prompt \
     or enable it in System Settings > Privacy & Security > Camera";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraConfig {
    pub index: u32,
    /// Close the stream (and the green LED) after this long without a capture.
    pub idle_timeout: Duration,
    /// Frames discarded after opening so autoexposure can settle.
    pub warmup_frames: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureOpts {
    pub max_width: Option<u32>,
    /// JPEG quality, clamped to 1..=100.
    pub quality: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub captured_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// Another capture is in flight; retry shortly.
    Busy,
    /// No device at the configured index, or camera permission denied.
    Unavailable {
        reason: String,
        retry_after: Duration,
    },
    /// The device exists but this attempt failed; retrying may succeed.
    Transient {
        reason: String,
        retry_after: Duration,
    },
}

impl CaptureError {
    /// How long until a retry can do anything: the camera's remaining backoff
    /// for `Unavailable` and `Transient`, a short constant for `Busy`.
    pub fn retry_after(&self) -> Duration {
        match self {
            CaptureError::Busy => BUSY_RETRY,
            CaptureError::Unavailable { retry_after, .. }
            | CaptureError::Transient { retry_after, .. } => *retry_after,
        }
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Busy => write!(f, "camera busy with another capture"),
            CaptureError::Unavailable { reason, .. } => write!(f, "camera unavailable: {reason}"),
            CaptureError::Transient { reason, .. } => write!(f, "camera capture failed: {reason}"),
        }
    }
}

impl std::error::Error for CaptureError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraState {
    Closed,
    Opening,
    Streaming,
    Failed { since: SystemTime, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraInfo {
    pub index: u32,
    pub name: String,
}

/// Cameras in the order nokhwa indexes them, so `index` matches `CameraConfig::index`.
pub fn list_cameras() -> Result<Vec<CameraInfo>, String> {
    let devices = catch_unwind(|| nokhwa::query(ApiBackend::AVFoundation))
        .map_err(|_| "camera query panicked".to_string())?
        .map_err(|e| e.to_string())?;
    Ok(devices
        .iter()
        .enumerate()
        .map(|(i, d)| CameraInfo {
            index: u32::try_from(i).unwrap_or(u32::MAX),
            name: d.human_name(),
        })
        .collect())
}

pub fn permission_granted() -> bool {
    nokhwa::nokhwa_check()
}

/// Shows the macOS camera prompt if the user has not decided yet, and waits up to
/// `timeout` for the answer. Returns immediately if access is already granted.
pub fn request_permission(timeout: Duration) -> bool {
    if nokhwa::nokhwa_check() {
        return true;
    }
    let (tx, rx) = mpsc::channel();
    nokhwa::nokhwa_initialize(move |granted| {
        let _ = tx.send(granted);
    });
    rx.recv_timeout(timeout).unwrap_or(false)
}

// ---------- lifecycle: pure bookkeeping, no nokhwa, unit-tested ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultKind {
    Unavailable,
    Transient,
}

/// A failure before the lifecycle has decided the backoff; becomes a
/// `CaptureError` once `retry_after` is known.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fault {
    kind: FaultKind,
    reason: String,
}

impl Fault {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            kind: FaultKind::Unavailable,
            reason: reason.into(),
        }
    }

    fn transient(reason: impl Into<String>) -> Self {
        Self {
            kind: FaultKind::Transient,
            reason: reason.into(),
        }
    }

    fn into_error(self, retry_after: Duration) -> CaptureError {
        let reason = self.reason;
        match self.kind {
            FaultKind::Unavailable => CaptureError::Unavailable {
                reason,
                retry_after,
            },
            FaultKind::Transient => CaptureError::Transient {
                reason,
                retry_after,
            },
        }
    }
}

/// What the worker should do with a capture request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Open,
    Shoot,
    Reject(CaptureError),
}

/// Lifecycle bookkeeping for one camera. Invariant kept by the worker:
/// `state == Streaming` exactly when it holds an open `Camera`.
#[derive(Debug, Clone)]
struct Lifecycle {
    state: CameraState,
    /// Consecutive failures since the last successful capture; drives backoff.
    failures: u32,
    retry_at: Option<Instant>,
    last_fault: Option<Fault>,
    last_used: Option<Instant>,
}

impl Lifecycle {
    fn new() -> Self {
        Self {
            state: CameraState::Closed,
            failures: 0,
            retry_at: None,
            last_fault: None,
            last_used: None,
        }
    }

    fn on_capture(&self, now: Instant) -> Step {
        match self.state {
            CameraState::Streaming => Step::Shoot,
            CameraState::Closed | CameraState::Opening => Step::Open,
            CameraState::Failed { .. } => {
                match self.retry_at.and_then(|t| t.checked_duration_since(now)) {
                    Some(wait) if !wait.is_zero() => {
                        let fault = self
                            .last_fault
                            .clone()
                            .unwrap_or_else(|| Fault::transient("camera failed"));
                        Step::Reject(fault.into_error(wait))
                    }
                    _ => Step::Open,
                }
            }
        }
    }

    fn on_opening(&mut self) {
        self.state = CameraState::Opening;
    }

    /// Failures are only forgiven by a delivered frame, not by a successful open,
    /// so a camera that opens but never streams still backs off.
    fn on_opened(&mut self, now: Instant) {
        self.state = CameraState::Streaming;
        self.last_used = Some(now);
    }

    fn on_shot(&mut self, now: Instant) {
        self.last_used = Some(now);
        self.failures = 0;
        self.retry_at = None;
        self.last_fault = None;
    }

    /// Records the failure, starts the next backoff and returns the error for the
    /// caller, carrying that backoff as `retry_after`.
    fn on_failure(&mut self, fault: Fault, now: Instant, wall: SystemTime) -> CaptureError {
        self.failures = self.failures.saturating_add(1);
        let wait = backoff(self.failures);
        self.retry_at = Some(now + wait);
        let err = fault.clone().into_error(wait);
        self.state = CameraState::Failed {
            since: wall,
            reason: err.to_string(),
        };
        self.last_fault = Some(fault);
        err
    }

    fn on_closed(&mut self) {
        self.state = CameraState::Closed;
    }

    /// When the open stream should be closed for inactivity; `None` unless streaming.
    fn idle_deadline(&self, idle_timeout: Duration) -> Option<Instant> {
        match self.state {
            CameraState::Streaming => self.last_used.map(|t| t + idle_timeout),
            _ => None,
        }
    }
}

/// 1s, 2s, 4s, ... capped at 30s, so a dead camera is not hammered.
fn backoff(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let factor = 1u32.checked_shl(failures - 1).unwrap_or(u32::MAX);
    BACKOFF_BASE.saturating_mul(factor).min(BACKOFF_MAX)
}

/// Target size for downscaling to `max_width`, preserving aspect ratio. Never upscales.
fn fit_width(width: u32, height: u32, max_width: Option<u32>) -> (u32, u32) {
    match max_width {
        Some(max) if max > 0 && width > max => {
            let h = (u64::from(height) * u64::from(max) + u64::from(width) / 2) / u64::from(width);
            (max, u32::try_from(h).unwrap_or(u32::MAX).max(1))
        }
        _ => (width, height),
    }
}

// ---------- handle ----------

struct Request {
    reply: mpsc::Sender<Result<Shot, CaptureError>>,
}

/// A raw frame handed back from the worker; decoding happens on the caller so
/// the worker can keep pumping and the timeout covers only the device.
struct Shot {
    buffer: Buffer,
    captured_at: SystemTime,
}

struct Worker {
    requests: mpsc::Sender<Request>,
    published: Arc<Mutex<Lifecycle>>,
}

impl Worker {
    fn spawn(cfg: &CameraConfig, lc: Lifecycle) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(lc.clone()));
        let worker_published = Arc::clone(&published);
        let cfg = cfg.clone();
        thread::Builder::new()
            .name("passfoto-camera".into())
            .spawn(move || run_worker(cfg, rx, worker_published, lc))?;
        Ok(Self {
            requests: tx,
            published,
        })
    }
}

struct Slot {
    /// `None` until the first capture, and again after a worker is abandoned.
    worker: Option<Worker>,
    /// Lifecycle to seed the next worker with, so backoff survives a respawn.
    carry: Lifecycle,
}

struct Inner {
    cfg: CameraConfig,
    slot: Mutex<Slot>,
    in_flight: Mutex<()>,
}

/// Send + Sync handle to the camera worker. Clones share one camera.
#[derive(Clone)]
pub struct CameraHandle {
    inner: Arc<Inner>,
}

impl CameraHandle {
    /// Creates the handle. No thread runs and no device opens until the first capture.
    pub fn spawn(cfg: CameraConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                cfg,
                slot: Mutex::new(Slot {
                    worker: None,
                    carry: Lifecycle::new(),
                }),
                in_flight: Mutex::new(()),
            }),
        }
    }

    /// Blocks until a fresh frame is captured and encoded, or fails. Returns
    /// `Busy` rather than queueing when another capture is already running.
    pub fn capture(&self, opts: CaptureOpts) -> Result<Frame, CaptureError> {
        let _guard = match self.inner.in_flight.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(CaptureError::Busy),
        };
        let shot = self.request_shot()?;
        encode(&shot, opts)
    }

    pub fn state(&self) -> CameraState {
        let slot = lock(&self.inner.slot);
        match &slot.worker {
            Some(w) => lock(&w.published).state.clone(),
            None => slot.carry.state.clone(),
        }
    }

    fn request_shot(&self) -> Result<Shot, CaptureError> {
        let requests = {
            let mut slot = lock(&self.inner.slot);
            if slot.worker.is_none() {
                let worker = Worker::spawn(&self.inner.cfg, slot.carry.clone())
                    .map_err(|e| retry_soon(format!("spawn camera thread: {e}")))?;
                slot.worker = Some(worker);
            }
            match &slot.worker {
                Some(w) => w.requests.clone(),
                None => return Err(retry_soon("camera thread missing")),
            }
        };
        let (reply, response) = mpsc::channel();
        let outcome = match requests.send(Request { reply }) {
            Ok(()) => response.recv_timeout(CAPTURE_TIMEOUT),
            Err(_) => Err(RecvTimeoutError::Disconnected),
        };
        match outcome {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(self.abandon_worker(format!(
                "no frame within {}s; the camera will be reopened",
                CAPTURE_TIMEOUT.as_secs()
            ))),
            Err(RecvTimeoutError::Disconnected) => {
                Err(self.abandon_worker("camera thread stopped unexpectedly".into()))
            }
        }
    }

    /// Gives up on a wedged or dead worker. A worker stuck inside nokhwa cannot be
    /// interrupted; dropping its request channel makes it close the camera and
    /// exit as soon as nokhwa returns. If the device never delivers another frame,
    /// that thread stays parked, which is the price of nokhwa's blocking API.
    fn abandon_worker(&self, why: String) -> CaptureError {
        let mut slot = lock(&self.inner.slot);
        if let Some(w) = slot.worker.take() {
            slot.carry = lock(&w.published).clone();
        }
        slot.carry
            .on_failure(Fault::transient(why), Instant::now(), SystemTime::now())
    }
}

/// A transient error outside the lifecycle (thread spawn, decode, encode), so no backoff.
fn retry_soon(reason: impl Into<String>) -> CaptureError {
    Fault::transient(reason).into_error(BUSY_RETRY)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ---------- worker thread: owns the nokhwa Camera ----------

fn run_worker(
    cfg: CameraConfig,
    rx: mpsc::Receiver<Request>,
    published: Arc<Mutex<Lifecycle>>,
    mut lc: Lifecycle,
) {
    let mut cam: Option<Camera> = None;
    loop {
        let msg = match lc.idle_deadline(cfg.idle_timeout) {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                Some(left) if !left.is_zero() => rx.recv_timeout(left.min(PUMP_INTERVAL)),
                _ => {
                    close(&mut cam);
                    lc.on_closed();
                    *lock(&published) = lc.clone();
                    continue;
                }
            },
        };
        match msg {
            Ok(req) => {
                let result = serve(&cfg, &mut cam, &mut lc, &published);
                *lock(&published) = lc.clone();
                let _ = req.reply.send(result);
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(c) = cam.as_mut() {
                    if let Err(e) = grab(c) {
                        close(&mut cam);
                        lc.on_failure(e, Instant::now(), SystemTime::now());
                        *lock(&published) = lc.clone();
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                close(&mut cam);
                return;
            }
        }
    }
}

fn serve(
    cfg: &CameraConfig,
    cam: &mut Option<Camera>,
    lc: &mut Lifecycle,
    published: &Mutex<Lifecycle>,
) -> Result<Shot, CaptureError> {
    match lc.on_capture(Instant::now()) {
        Step::Reject(e) => return Err(e),
        Step::Shoot => {}
        Step::Open => {
            lc.on_opening();
            *lock(published) = lc.clone();
            match open_warm(cfg) {
                Ok(c) => {
                    *cam = Some(c);
                    lc.on_opened(Instant::now());
                }
                Err(fault) => {
                    return Err(lc.on_failure(fault, Instant::now(), SystemTime::now()));
                }
            }
        }
    }
    let result = match cam.as_mut() {
        Some(c) => fresh_frame(c),
        None => Err(Fault::transient("camera not open")),
    };
    match result {
        Ok(shot) => {
            lc.on_shot(Instant::now());
            Ok(shot)
        }
        Err(fault) => {
            close(cam);
            Err(lc.on_failure(fault, Instant::now(), SystemTime::now()))
        }
    }
}

/// Runs a nokhwa call, turning both its errors and its panics into `Transient`.
fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T, NokhwaError>) -> Result<T, Fault> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(Fault::transient(format!("{what}: {e}"))),
        Err(_) => Err(Fault::transient(format!("{what}: camera driver panicked"))),
    }
}

fn open_warm(cfg: &CameraConfig) -> Result<Camera, Fault> {
    if !nokhwa::nokhwa_check() {
        // Shows the prompt if the user has not decided yet; a no-op once denied.
        nokhwa::nokhwa_initialize(|_| {});
        return Err(Fault::unavailable(PERMISSION_HINT));
    }
    let found = list_cameras().map_err(Fault::transient)?;
    if !found.iter().any(|c| c.index == cfg.index) {
        return Err(Fault::unavailable(format!(
            "no camera at index {} ({} found)",
            cfg.index,
            found.len()
        )));
    }
    let mut cam = guarded("open camera", || {
        Camera::new(
            CameraIndex::Index(cfg.index),
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution),
        )
    })
    .map_err(explain_lock)?;
    let warmed = guarded("open stream", || cam.open_stream())
        .and_then(|()| (0..cfg.warmup_frames).try_for_each(|_| grab(&mut cam)));
    match warmed {
        Ok(()) => Ok(cam),
        Err(e) => {
            close(&mut Some(cam));
            Err(e)
        }
    }
}

/// AVFoundation refuses `lockForConfiguration` while another process holds the
/// device, and nokhwa surfaces that as an opaque property error.
fn explain_lock(fault: Fault) -> Fault {
    if fault.reason.contains("lockForConfiguration") {
        Fault::transient(format!(
            "camera is in use by another app ({})",
            fault.reason
        ))
    } else {
        fault
    }
}

/// Pulls one frame and throws it away; also drains whatever else is queued.
fn grab(cam: &mut Camera) -> Result<(), Fault> {
    guarded("read frame", || cam.frame()).map(drop)
}

/// The first `frame()` returns the oldest queued frame and drains the rest, so
/// the second one is the next frame the sensor delivers.
fn fresh_frame(cam: &mut Camera) -> Result<Shot, Fault> {
    grab(cam)?;
    let buffer = guarded("read frame", || cam.frame())?;
    Ok(Shot {
        buffer,
        captured_at: SystemTime::now(),
    })
}

/// nokhwa's `Drop for Camera` unwraps `stop_stream()`, which can panic on a
/// device that has gone away; stop explicitly and contain the drop.
fn close(cam: &mut Option<Camera>) {
    if let Some(mut c) = cam.take() {
        let _ = catch_unwind(AssertUnwindSafe(move || {
            let _ = c.stop_stream();
            drop(c);
        }));
    }
}

fn encode(shot: &Shot, opts: CaptureOpts) -> Result<Frame, CaptureError> {
    let mut img = shot
        .buffer
        .decode_image::<RgbFormat>()
        .map_err(|e| retry_soon(format!("decode: {e}")))?;
    let (w, h) = fit_width(img.width(), img.height(), opts.max_width);
    if w != img.width() {
        img = image::imageops::resize(&img, w, h, FilterType::Triangle);
    }
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, opts.quality.clamp(1, 100))
        .encode_image(&img)
        .map_err(|e| retry_soon(format!("encode: {e}")))?;
    Ok(Frame {
        jpeg,
        width: w,
        height: h,
        captured_at: shot.captured_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(30);

    fn wall() -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    #[test]
    fn fit_width_downscales_preserving_aspect() {
        assert_eq!(fit_width(1920, 1080, Some(1280)), (1280, 720));
        assert_eq!(fit_width(4032, 3024, Some(1000)), (1000, 750));
        assert_eq!(fit_width(1000, 333, Some(500)), (500, 167)); // rounds to nearest
    }

    #[test]
    fn fit_width_never_upscales_or_divides_by_zero() {
        assert_eq!(fit_width(640, 480, Some(1280)), (640, 480));
        assert_eq!(fit_width(640, 480, Some(640)), (640, 480));
        assert_eq!(fit_width(640, 480, None), (640, 480));
        assert_eq!(fit_width(640, 480, Some(0)), (640, 480));
        assert_eq!(fit_width(10_000, 1, Some(10)), (10, 1));
    }

    #[test]
    fn backoff_doubles_then_caps() {
        assert_eq!(backoff(0), Duration::ZERO);
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(5), Duration::from_secs(16));
        assert_eq!(backoff(6), BACKOFF_MAX);
        assert_eq!(backoff(u32::MAX), BACKOFF_MAX);
    }

    #[test]
    fn closed_camera_opens_lazily_then_shoots_while_streaming() {
        let now = Instant::now();
        let mut lc = Lifecycle::new();
        assert_eq!(lc.on_capture(now), Step::Open);
        assert_eq!(lc.idle_deadline(IDLE), None);
        lc.on_opening();
        assert_eq!(lc.state, CameraState::Opening);
        lc.on_opened(now);
        lc.on_shot(now);
        assert_eq!(lc.state, CameraState::Streaming);
        assert_eq!(lc.on_capture(now), Step::Shoot);
    }

    #[test]
    fn idle_deadline_follows_last_capture_and_clears_on_close() {
        let t0 = Instant::now();
        let mut lc = Lifecycle::new();
        lc.on_opened(t0);
        assert_eq!(lc.idle_deadline(IDLE), Some(t0 + IDLE));
        let t1 = t0 + Duration::from_secs(10);
        lc.on_shot(t1);
        assert_eq!(lc.idle_deadline(IDLE), Some(t1 + IDLE));
        lc.on_closed();
        assert_eq!(lc.state, CameraState::Closed);
        assert_eq!(lc.idle_deadline(IDLE), None);
        assert_eq!(lc.on_capture(t1), Step::Open);
    }

    #[test]
    fn failure_returns_backoff_and_rejects_with_original_kind_until_it_expires() {
        let t0 = Instant::now();
        let mut lc = Lifecycle::new();
        let fault = Fault::unavailable("no camera at index 0 (0 found)");
        let err = lc.on_failure(fault, t0, wall());
        assert_eq!(
            err,
            CaptureError::Unavailable {
                reason: "no camera at index 0 (0 found)".into(),
                retry_after: Duration::from_secs(1),
            }
        );
        assert!(
            matches!(lc.state, CameraState::Failed { ref reason, .. } if reason.contains("index 0"))
        );
        assert_eq!(lc.idle_deadline(IDLE), None);
        match lc.on_capture(t0 + Duration::from_millis(200)) {
            Step::Reject(e @ CaptureError::Unavailable { .. }) => {
                assert_eq!(e.retry_after(), Duration::from_millis(800));
            }
            other => panic!("expected Unavailable rejection, got {other:?}"),
        }
        assert_eq!(lc.on_capture(t0 + Duration::from_secs(1)), Step::Open);
    }

    #[test]
    fn repeated_failures_back_off_longer_until_a_frame_arrives() {
        let t0 = Instant::now();
        let mut lc = Lifecycle::new();
        let gone = || Fault::transient("read frame: gone");
        assert_eq!(
            lc.on_failure(gone(), t0, wall()).retry_after(),
            Duration::from_secs(1)
        );
        assert_eq!(
            lc.on_failure(gone(), t0, wall()).retry_after(),
            Duration::from_secs(2)
        );
        match lc.on_capture(t0 + Duration::from_millis(1500)) {
            Step::Reject(e @ CaptureError::Transient { .. }) => {
                assert_eq!(e.retry_after(), Duration::from_millis(500));
            }
            other => panic!("expected Transient rejection, got {other:?}"),
        }
        assert_eq!(lc.on_capture(t0 + Duration::from_secs(2)), Step::Open);

        // A successful open alone does not forgive earlier failures.
        lc.on_opened(t0);
        assert_eq!(
            lc.on_failure(gone(), t0, wall()).retry_after(),
            Duration::from_secs(4)
        );

        lc.on_opened(t0);
        lc.on_shot(t0);
        assert_eq!(lc.failures, 0);
        assert_eq!(
            lc.on_failure(gone(), t0, wall()).retry_after(),
            Duration::from_secs(1)
        );
        assert_eq!(lc.on_capture(t0 + Duration::from_secs(1)), Step::Open);
    }

    #[test]
    fn busy_and_out_of_lifecycle_errors_retry_soon() {
        assert_eq!(CaptureError::Busy.retry_after(), BUSY_RETRY);
        assert_eq!(retry_soon("decode: bad").retry_after(), BUSY_RETRY);
    }

    #[test]
    fn lock_rejection_is_explained_as_camera_in_use() {
        let raw = Fault::transient("open camera: Could not set device property lockForConfiguration with value Locked: Lock Rejected");
        let explained = explain_lock(raw);
        assert_eq!(explained.kind, FaultKind::Transient);
        assert!(explained
            .reason
            .starts_with("camera is in use by another app"));
        let other = Fault::transient("open stream: nope");
        assert_eq!(explain_lock(other.clone()), other);
    }

    #[test]
    fn spawn_starts_closed_without_touching_hardware() {
        let cam = CameraHandle::spawn(CameraConfig {
            index: 0,
            idle_timeout: IDLE,
            warmup_frames: 0,
        });
        assert_eq!(cam.state(), CameraState::Closed);
    }

    #[test]
    fn handle_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CameraHandle>();
    }

    /// Needs a real camera and permission: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn hardware_capture_opens_resizes_and_closes_when_idle() {
        let cam = CameraHandle::spawn(CameraConfig {
            index: 0,
            idle_timeout: Duration::from_secs(1),
            warmup_frames: 15,
        });
        let opts = CaptureOpts {
            max_width: Some(640),
            quality: 80,
        };
        let frame = cam.capture(opts).expect("first capture");
        assert_eq!(frame.width, 640);
        assert!(frame.jpeg.starts_with(&[0xFF, 0xD8]));
        assert_eq!(cam.state(), CameraState::Streaming);
        let again = cam.capture(opts).expect("second capture");
        assert!(again.captured_at > frame.captured_at);
        thread::sleep(Duration::from_millis(1600));
        assert_eq!(cam.state(), CameraState::Closed);
    }
}
