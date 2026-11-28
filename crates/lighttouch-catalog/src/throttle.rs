//! The one gate every Legacy Store request passes, so the app stays a polite client of a small volunteer-run
//! service (it answers bursts with 429). Same rules as the Android app's StoreThrottle.kt: at most 8 requests in
//! flight, 16 a second with bursts of 16, and a 429 (or a 503 that says when to come back) pauses every request for
//! as long as the server asks.
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const MAX_IN_FLIGHT: usize = 8;
const INTERVAL: Duration = Duration::from_micros(62_500);
const BURST: u32 = 16;
/// A 429 without a usable Retry-After.
const DEFAULT_PAUSE: Duration = Duration::from_secs(30);
const MAX_PAUSE: Duration = Duration::from_secs(600);

#[derive(Default)]
struct State {
    in_flight: usize,
    /// When the next request would start if requests went out exactly one interval apart (GCRA).
    next: Option<Instant>,
    paused_until: Option<Instant>,
}

impl State {
    /// How long a request starting at `now` waits for its turn; reserves that turn.
    fn reserve(&mut self, now: Instant) -> Duration {
        let next = self.next.map_or(now, |n| n.max(now));
        self.next = Some(next + INTERVAL);
        next.saturating_duration_since(now).saturating_sub(INTERVAL * (BURST - 1))
    }

    fn paused(&self, now: Instant) -> Option<Duration> {
        self.paused_until.filter(|u| *u > now).map(|u| u - now)
    }

    fn pause(&mut self, now: Instant, retry_after: Option<Duration>) {
        let until = now + retry_after.unwrap_or(DEFAULT_PAUSE).clamp(Duration::from_secs(1), MAX_PAUSE);
        self.paused_until = Some(self.paused_until.map_or(until, |u| u.max(until)));
    }
}

static STATE: Mutex<State> = Mutex::new(State { in_flight: 0, next: None, paused_until: None });
static CHANGED: Condvar = Condvar::new();

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A request's place in flight, given back when dropped.
pub(crate) struct Permit;

impl Drop for Permit {
    fn drop(&mut self) {
        state().in_flight -= 1;
        CHANGED.notify_all();
    }
}

/// Wait for a turn to send one request. `patient` requests (icons, screenshots, loaded in the background) also wait
/// out a pause; the others fail at once with the time left, so the person sees why instead of a stalled screen.
pub(crate) fn enter(patient: bool) -> Result<Permit, Duration> {
    let mut s = state();
    loop {
        let now = Instant::now();
        if let Some(left) = s.paused(now) {
            if !patient {
                return Err(left);
            }
            s = CHANGED.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        } else if s.in_flight >= MAX_IN_FLIGHT {
            s = CHANGED.wait(s).unwrap_or_else(|e| e.into_inner());
        } else {
            break;
        }
    }
    let delay = s.reserve(Instant::now());
    s.in_flight += 1;
    drop(s);
    let permit = Permit;
    std::thread::sleep(delay);
    // A pause that began while this one waited for its turn still applies.
    let mut s = state();
    while let Some(left) = s.paused(Instant::now()) {
        if !patient {
            return Err(left);
        }
        s = CHANGED.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
    }
    Ok(permit)
}

/// Note an answer's status: a 429, or a 503 with Retry-After, pauses every request. Returns the pause left, if any.
pub(crate) fn answered(status: u16, retry_after: Option<&str>) -> Option<Duration> {
    let seconds = retry_after.and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_secs);
    if status == 429 || (status == 503 && retry_after.is_some()) {
        let mut s = state();
        s.pause(Instant::now(), seconds);
        return s.paused(Instant::now());
    }
    None
}

/// What the person reads during a pause.
pub(crate) fn slow_down(left: Duration) -> String {
    let seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    format!("Legacy Store asked LightTouch to slow down. Try again in {} seconds.", seconds.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_goes_at_once_then_one_per_interval() {
        let mut s = State::default();
        let now = Instant::now();
        for _ in 0..BURST {
            assert_eq!(s.reserve(now), Duration::ZERO);
        }
        assert_eq!(s.reserve(now), INTERVAL);
        assert_eq!(s.reserve(now), INTERVAL * 2);
        // Quiet time earns the burst back.
        let later = now + INTERVAL * (BURST + 2) * 2;
        assert_eq!(s.reserve(later), Duration::ZERO);
    }

    #[test]
    fn a_pause_follows_the_server_within_limits() {
        let mut s = State::default();
        let now = Instant::now();
        s.pause(now, None);
        assert_eq!(s.paused(now), Some(DEFAULT_PAUSE));
        s.pause(now, Some(Duration::from_secs(5)));
        assert_eq!(s.paused(now), Some(DEFAULT_PAUSE), "a shorter answer doesn't cut a pause short");
        s.pause(now, Some(Duration::from_secs(86_400)));
        assert_eq!(s.paused(now), Some(MAX_PAUSE));
        assert_eq!(s.paused(now + MAX_PAUSE), None);
    }

    #[test]
    fn the_message_rounds_up() {
        assert_eq!(
            slow_down(Duration::from_millis(1500)),
            "Legacy Store asked LightTouch to slow down. Try again in 2 seconds."
        );
    }
}
