use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Agent cooldown shared by every server thread.
///
/// A request first reserves the slot (atomically, under the lock) and only then
/// captures. A pending reservation counts as a shot, so a concurrent agent request
/// is refused. If the capture fails, dropping the reservation releases the slot
/// and the cooldown is as if the request never happened.
pub struct Throttle {
    min_interval: Duration,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    last_shot: Option<Instant>,
    pending: Option<(u64, Instant)>,
    next_id: u64,
}

impl State {
    fn busy_since(&self) -> Option<Instant> {
        match (self.last_shot, self.pending) {
            (Some(a), Some((_, b))) => Some(a.max(b)),
            (a, b) => a.or(b.map(|(_, t)| t)),
        }
    }
}

pub struct Reservation<'a> {
    throttle: &'a Throttle,
    id: u64,
    committed: bool,
}

impl Reservation<'_> {
    /// Record the shot; the cooldown starts at `at`.
    pub fn commit(mut self, at: Instant) {
        let mut s = self.throttle.lock();
        if s.pending.is_some_and(|(id, _)| id == self.id) {
            s.pending = None;
        }
        s.last_shot = Some(s.last_shot.map_or(at, |t| t.max(at)));
        self.committed = true;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut s = self.throttle.lock();
        if s.pending.is_some_and(|(id, _)| id == self.id) {
            s.pending = None;
        }
    }
}

/// Snapshot of the throttle for `/status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleStatus {
    pub cooldown_remaining: Duration,
    pub last_shot_age: Option<Duration>,
}

impl Throttle {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            state: Mutex::new(State::default()),
        }
    }

    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // The state is a few plain values that are never left half-updated, so a
        // panic elsewhere while holding the lock does not make it unusable.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ok(reservation) if a shot may start now, Err(wait) with the remaining cooldown otherwise.
    pub fn try_reserve(&self, now: Instant) -> Result<Reservation<'_>, Duration> {
        let mut s = self.lock();
        if let Some(wait) = remaining(s.busy_since(), self.min_interval, now) {
            return Err(wait);
        }
        let id = s.next_id;
        s.next_id = s.next_id.wrapping_add(1);
        s.pending = Some((id, now));
        Ok(Reservation {
            throttle: self,
            id,
            committed: false,
        })
    }

    pub fn status(&self, now: Instant) -> ThrottleStatus {
        let s = self.lock();
        ThrottleStatus {
            cooldown_remaining: remaining(s.busy_since(), self.min_interval, now)
                .unwrap_or_default(),
            last_shot_age: s.last_shot.map(|t| now.saturating_duration_since(t)),
        }
    }
}

fn remaining(since: Option<Instant>, min_interval: Duration, now: Instant) -> Option<Duration> {
    let elapsed = now.saturating_duration_since(since?);
    min_interval.checked_sub(elapsed).filter(|w| !w.is_zero())
}

/// Whole seconds to wait, rounded up so a client that waits this long is never early.
pub fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IVL: Duration = Duration::from_secs(120);

    #[test]
    fn first_shot_is_allowed() {
        let t = Throttle::new(IVL);
        assert!(t.try_reserve(Instant::now()).is_ok());
    }

    #[test]
    fn committed_shot_blocks_until_interval_passes() {
        let t = Throttle::new(IVL);
        let t0 = Instant::now();
        t.try_reserve(t0).unwrap().commit(t0);
        let wait = t.try_reserve(t0 + Duration::from_secs(30)).err().unwrap();
        assert_eq!(wait, Duration::from_secs(90));
        assert!(t.try_reserve(t0 + IVL).is_ok());
    }

    #[test]
    fn pending_reservation_blocks_a_concurrent_request() {
        let t = Throttle::new(IVL);
        let t0 = Instant::now();
        let held = t.try_reserve(t0).unwrap();
        assert!(t.try_reserve(t0 + Duration::from_millis(5)).is_err());
        drop(held);
    }

    #[test]
    fn failed_capture_releases_the_slot_and_keeps_old_cooldown() {
        let t = Throttle::new(IVL);
        let t0 = Instant::now();
        t.try_reserve(t0).unwrap().commit(t0);
        let later = t0 + IVL + Duration::from_secs(1);
        drop(t.try_reserve(later).unwrap());
        assert!(t.try_reserve(later).is_ok());
        let st = t.status(later);
        assert_eq!(st.cooldown_remaining, Duration::ZERO);
        assert_eq!(st.last_shot_age, Some(IVL + Duration::from_secs(1)));
    }

    #[test]
    fn concurrent_threads_get_exactly_one_reservation() {
        let t = Throttle::new(IVL);
        let now = Instant::now();
        let winners: usize = std::thread::scope(|s| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    s.spawn(|| match t.try_reserve(now) {
                        Ok(r) => {
                            r.commit(now);
                            1
                        }
                        Err(_) => 0,
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(winners, 1);
    }

    #[test]
    fn zero_interval_never_throttles() {
        let t = Throttle::new(Duration::ZERO);
        let now = Instant::now();
        let a = t.try_reserve(now).unwrap();
        let b = t.try_reserve(now).unwrap();
        a.commit(now);
        b.commit(now);
    }

    #[test]
    fn status_reports_cooldown_and_age() {
        let t = Throttle::new(IVL);
        let t0 = Instant::now();
        assert_eq!(
            t.status(t0),
            ThrottleStatus {
                cooldown_remaining: Duration::ZERO,
                last_shot_age: None
            }
        );
        t.try_reserve(t0).unwrap().commit(t0);
        let st = t.status(t0 + Duration::from_secs(20));
        assert_eq!(st.cooldown_remaining, Duration::from_secs(100));
        assert_eq!(st.last_shot_age, Some(Duration::from_secs(20)));
    }

    #[test]
    fn ceil_secs_rounds_up() {
        assert_eq!(ceil_secs(Duration::from_secs(90)), 90);
        assert_eq!(ceil_secs(Duration::from_millis(89_001)), 90);
        assert_eq!(ceil_secs(Duration::from_millis(1)), 1);
        assert_eq!(ceil_secs(Duration::ZERO), 0);
    }
}
