//! `process::start_while_busy` on a clock of its own (package review r1, R1-01: "bound sleeps by remaining busy/overall
//! budgets and check those budgets before subsequent spawn attempts; retain the last busy error when refusing"; "a
//! controlled clock/spawn seam" rather than the scheduler). Off Windows a busy refusal (ETXTBSY) is tried again; on
//! Windows, which has no such refusal, every result is the answer at once.

use lords_core::process::start_while_busy;
use std::cell::{Cell, RefCell};
use std::io::{Error, ErrorKind};
use std::time::{Duration, Instant};

/// A clock that moves only when the code under test sleeps, and every start and pause it saw.
struct Clock {
    t0: Instant,
    now: Cell<Instant>,
    starts: RefCell<Vec<Duration>>,
    pauses: RefCell<Vec<Duration>>,
}

impl Clock {
    fn new() -> Clock {
        let t0 = Instant::now();
        Clock { t0, now: Cell::new(t0), starts: RefCell::default(), pauses: RefCell::default() }
    }

    /// `start_while_busy` with `outcome(n)` for the n-th start, until `until` after the clock's start.
    fn run(
        &self,
        until: Duration,
        outcome: impl Fn(usize) -> std::io::Result<u32>,
    ) -> std::io::Result<u32> {
        let spawn = || {
            let n = self.starts.borrow().len();
            self.starts.borrow_mut().push(self.now.get() - self.t0);
            outcome(n)
        };
        let sleep = |d: Duration| {
            self.pauses.borrow_mut().push(d);
            self.now.set(self.now.get() + d);
        };
        start_while_busy(spawn, self.t0 + until, || self.now.get(), sleep)
    }
}

fn busy(n: usize) -> Error {
    Error::new(ErrorKind::ExecutableFileBusy, format!("busy {n}"))
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// Spec (the job lords-test-linux-84c9449e-1794157526-435833; the master, 18:36: retried only on that refusal, off
// Windows): a start refused as busy five times, then started, is the started one, after five 20 ms pauses.
#[test]
fn a_busy_start_is_tried_again_until_it_starts() {
    let c = Clock::new();
    let r = c.run(ms(1000), |n| if n < 5 { Err(busy(n)) } else { Ok(7) });
    if cfg!(unix) {
        assert_eq!(r.unwrap(), 7);
        assert_eq!(c.starts.borrow().len(), 6);
        assert_eq!(*c.pauses.borrow(), vec![ms(20); 5]);
    } else {
        assert_eq!(r.unwrap_err().to_string(), "busy 0", "Windows: the answer at once");
        assert_eq!(c.starts.borrow().len(), 1);
    }
}

// Spec (R1-01: no start past the window, the last busy error kept): refused as busy every time with 50 ms to go, the
// starts are at 0, 20 and 40 ms, the last pause ends exactly at the end (10 ms), no start is tried there, and the
// answer is the last refusal.
#[test]
fn no_start_is_tried_at_or_past_the_end_and_the_last_refusal_is_the_answer() {
    let c = Clock::new();
    let r = c.run(ms(50), |n| Err(busy(n)));
    if cfg!(unix) {
        assert_eq!(r.unwrap_err().to_string(), "busy 2");
        assert_eq!(*c.starts.borrow(), vec![ms(0), ms(20), ms(40)]);
        assert_eq!(*c.pauses.borrow(), vec![ms(20), ms(20), ms(10)]);
    } else {
        assert_eq!(c.starts.borrow().len(), 1);
    }
}

// Spec (R1-01: the window inside the overall budget): with no time left, a busy start is the answer at once, with no
// pause.
#[test]
fn with_no_time_left_a_busy_start_is_the_answer_at_once() {
    let c = Clock::new();
    let r = c.run(ms(0), |n| Err(busy(n)));
    assert_eq!(r.unwrap_err().to_string(), "busy 0");
    assert_eq!(c.starts.borrow().len(), 1);
    assert!(c.pauses.borrow().is_empty());
}

// Spec (the master, 18:36: "Any other error … fails exactly as today"): any other failure is the answer at once.
#[test]
fn any_other_failure_is_the_answer_at_once() {
    let c = Clock::new();
    let r = c.run(ms(1000), |_| Err(Error::new(ErrorKind::NotFound, "gone")));
    assert_eq!(r.unwrap_err().kind(), ErrorKind::NotFound);
    assert_eq!(c.starts.borrow().len(), 1);
    assert!(c.pauses.borrow().is_empty());
}
