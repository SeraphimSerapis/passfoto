//! Webcam access behind a dedicated thread.
//!
//! The nokhwa `Camera` never leaves its thread; callers hold a cheap, cloneable
//! `CameraHandle` and talk to that thread over channels.

use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, SystemTime};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::{Buffer, Camera};

/// Upper bound for one capture as seen by the caller, including open and warmup.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

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
    Unavailable(String),
    /// The device exists but this attempt failed; retrying may succeed.
    Transient(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Busy => write!(f, "camera busy with another capture"),
            CaptureError::Unavailable(why) => write!(f, "camera unavailable: {why}"),
            CaptureError::Transient(why) => write!(f, "camera capture failed: {why}"),
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
    let devices = nokhwa::query(ApiBackend::AVFoundation).map_err(|e| e.to_string())?;
    Ok(devices
        .iter()
        .enumerate()
        .map(|(i, d)| CameraInfo {
            index: u32::try_from(i).unwrap_or(u32::MAX),
            name: d.human_name(),
        })
        .collect())
}

struct Request {
    reply: mpsc::Sender<Result<Shot, CaptureError>>,
}

/// A raw frame handed back from the camera thread; decoding happens on the caller.
struct Shot {
    buffer: Buffer,
    captured_at: SystemTime,
}

struct Inner {
    requests: mpsc::Sender<Request>,
    state: Arc<Mutex<CameraState>>,
    in_flight: Mutex<()>,
}

/// Send + Sync handle to the camera thread. Clones share one camera.
#[derive(Clone)]
pub struct CameraHandle {
    inner: Arc<Inner>,
}

impl CameraHandle {
    /// Starts the camera thread. The device is not opened until the first capture.
    pub fn spawn(cfg: CameraConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(CameraState::Closed));
        let worker_state = Arc::clone(&state);
        thread::Builder::new()
            .name("passfoto-camera".into())
            .spawn(move || worker(cfg, rx, worker_state))
            .expect("spawn camera thread");
        Self {
            inner: Arc::new(Inner {
                requests: tx,
                state,
                in_flight: Mutex::new(()),
            }),
        }
    }

    /// Blocks until a fresh frame is captured and encoded, or fails.
    pub fn capture(&self, opts: CaptureOpts) -> Result<Frame, CaptureError> {
        let _guard = match self.inner.in_flight.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(CaptureError::Busy),
        };
        let (reply, response) = mpsc::channel();
        self.inner
            .requests
            .send(Request { reply })
            .map_err(|_| CaptureError::Transient("camera thread stopped".into()))?;
        let shot = match response.recv_timeout(CAPTURE_TIMEOUT) {
            Ok(r) => r?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(CaptureError::Transient("camera timed out".into()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(CaptureError::Transient("camera thread stopped".into()))
            }
        };
        encode(&shot, opts)
    }

    pub fn state(&self) -> CameraState {
        lock(&self.inner.state).clone()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn worker(cfg: CameraConfig, rx: mpsc::Receiver<Request>, state: Arc<Mutex<CameraState>>) {
    let mut cam: Option<Camera> = None;
    for req in rx {
        if cam.is_none() {
            *lock(&state) = CameraState::Opening;
            match open(cfg.index) {
                Ok(c) => {
                    cam = Some(c);
                    *lock(&state) = CameraState::Streaming;
                }
                Err(e) => {
                    *lock(&state) = CameraState::Failed {
                        since: SystemTime::now(),
                        reason: e.to_string(),
                    };
                    let _ = req.reply.send(Err(e));
                    continue;
                }
            }
        }
        let Some(c) = cam.as_mut() else { continue };
        let result = c
            .frame()
            .map(|buffer| Shot {
                buffer,
                captured_at: SystemTime::now(),
            })
            .map_err(|e| CaptureError::Transient(format!("capture: {e}")));
        let _ = req.reply.send(result);
    }
}

fn open(index: u32) -> Result<Camera, CaptureError> {
    let mut cam = Camera::new(
        CameraIndex::Index(index),
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution),
    )
    .map_err(|e| CaptureError::Unavailable(e.to_string()))?;
    cam.open_stream()
        .map_err(|e| CaptureError::Transient(format!("open stream: {e}")))?;
    Ok(cam)
}

fn encode(shot: &Shot, opts: CaptureOpts) -> Result<Frame, CaptureError> {
    let mut img = shot
        .buffer
        .decode_image::<RgbFormat>()
        .map_err(|e| CaptureError::Transient(format!("decode: {e}")))?;
    let (w, h) = fit_width(img.width(), img.height(), opts.max_width);
    if w != img.width() {
        img = image::imageops::resize(&img, w, h, FilterType::Triangle);
    }
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, opts.quality.clamp(1, 100))
        .encode_image(&img)
        .map_err(|e| CaptureError::Transient(format!("encode: {e}")))?;
    Ok(Frame {
        jpeg,
        width: w,
        height: h,
        captured_at: shot.captured_at,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_width_downscales_preserving_aspect() {
        assert_eq!(fit_width(1920, 1080, Some(1280)), (1280, 720));
        assert_eq!(fit_width(4032, 3024, Some(1000)), (1000, 750));
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
    fn handle_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CameraHandle>();
    }
}
