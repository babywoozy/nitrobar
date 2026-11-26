//! "Nitro", the little flame that lives on the left of the resting island: its
//! sleep state and the shapes it is drawn from (an original design: a nitrous
//! flame with two eyes).
//!
//! It dozes off when the machine has been idle (a one-shot timer checked against
//! `GetLastInputInfo`, no polling) and wakes with a little hop when the pointer
//! comes to the island or the active app changes. The floating "z" animates for a
//! short while only and then holds still, so a sleeping machine costs no CPU.

use std::time::{Duration, Instant};

use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

use crate::morph::spring;

/// Interval of the floating "z" while falling asleep, and how many steps it runs.
pub const Z_STEP_MS: u32 = 450;
pub const Z_STEPS: u32 = 24;
/// Length of the wake-up hop.
pub const WAKE: Duration = Duration::from_millis(500);

/// Milliseconds since the last keyboard / mouse input.
pub fn idle_millis() -> u32 {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    unsafe {
        if !GetLastInputInfo(&raw mut info).as_bool() {
            return 0;
        }
        GetTickCount().wrapping_sub(info.dwTime)
    }
}

#[derive(Default)]
pub struct Mascot {
    asleep: bool,
    woke: Option<Instant>,
    z_step: u32,
}

impl Mascot {
    pub fn asleep(&self) -> bool {
        self.asleep
    }

    pub fn sleep(&mut self) {
        self.asleep = true;
        self.woke = None;
        self.z_step = 0;
    }

    /// Wakes up; returns true if it was asleep (so it should be redrawn).
    pub fn wake(&mut self, now: Instant) -> bool {
        if !self.asleep {
            return false;
        }
        self.asleep = false;
        self.woke = Some(now);
        true
    }

    /// One step of the floating "z". False once it has settled.
    pub fn step_z(&mut self) -> bool {
        if !self.asleep || self.z_step >= Z_STEPS {
            return false;
        }
        self.z_step += 1;
        self.z_step < Z_STEPS
    }

    /// Where the "z" is in its rise, 0..1 (`None` while awake). Settles at 0.5.
    pub fn z_phase(&self) -> Option<f32> {
        self.asleep.then(|| {
            if self.z_step >= Z_STEPS {
                0.5
            } else {
                (self.z_step % 4) as f32 / 4.0
            }
        })
    }

    pub fn waking(&self, now: Instant) -> bool {
        self.woke
            .is_some_and(|at| now.saturating_duration_since(at) < WAKE)
    }

    /// Size factor: 1 at rest, a springy pop right after waking.
    pub fn scale(&self, now: Instant) -> f32 {
        match self.woke {
            Some(at) if self.waking(now) => {
                let t = now.saturating_duration_since(at).as_secs_f32();
                0.72 + 0.28 * spring(t, 24.0, 0.42)
            }
            _ => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleeps_animates_a_while_then_holds_still() {
        let mut m = Mascot::default();
        assert!(m.z_phase().is_none() && !m.step_z());
        m.sleep();
        let mut steps = 0;
        while m.step_z() {
            steps += 1;
        }
        assert_eq!(steps, Z_STEPS - 1);
        assert_eq!(m.z_phase(), Some(0.5));
    }

    #[test]
    fn wakes_with_a_hop_that_ends() {
        let mut m = Mascot::default();
        let t = Instant::now();
        assert!(!m.wake(t), "already awake");
        m.sleep();
        assert!(m.wake(t) && !m.asleep());
        assert!(m.scale(t) < 0.8);
        let peak = (0..50)
            .map(|i| m.scale(t + Duration::from_millis(i * 10)))
            .fold(0.0f32, f32::max);
        assert!(peak > 1.02, "overshoots: {peak}");
        assert_eq!(m.scale(t + WAKE), 1.0);
        assert!(!m.waking(t + WAKE));
    }
}
