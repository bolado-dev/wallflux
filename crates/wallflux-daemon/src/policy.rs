//! When to draw, and how fast.
//!
//! Kept separate from the engine and free of any window-system types, because
//! this is the part of wallflux most worth being sure about: every rule here
//! is the difference between a wallpaper that costs nothing when nobody can
//! see it and one that burns a core all day.

use std::time::{Duration, Instant};

use wallflux_core::config::PowerConfig;
use wallflux_core::ipc::PauseReason;

use crate::power::PowerSource;

/// Everything known about one output at decision time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signals {
    /// False when the window system says the wallpaper cannot be seen.
    pub visible: bool,
    /// A fullscreen window is focused on this output.
    pub fullscreen: bool,
    /// Someone asked for a pause over IPC.
    pub manual: bool,
    /// An output with nothing assigned has nothing to draw.
    pub has_wallpaper: bool,
    pub power: PowerSource,
}

impl Default for Signals {
    fn default() -> Self {
        Self {
            visible: true,
            fullscreen: false,
            manual: false,
            has_wallpaper: true,
            power: PowerSource::Ac,
        }
    }
}

/// Why this output should not be drawing, or `None` to draw.
///
/// Order matters: the reason reported is the most specific one, because it is
/// what `wallfluxctl status` shows and "paused: occluded" is a more useful
/// answer than "paused: battery" when both are true.
pub fn pause_reason(signals: &Signals, config: &PowerConfig) -> Option<PauseReason> {
    if !signals.has_wallpaper {
        return Some(PauseReason::Idle);
    }
    if signals.manual {
        return Some(PauseReason::Manual);
    }
    if config.pause_on_fullscreen && signals.fullscreen {
        return Some(PauseReason::Fullscreen);
    }
    if config.pause_on_occlusion && !signals.visible {
        return Some(PauseReason::Occluded);
    }
    if config.pause_on_battery && signals.power == PowerSource::Battery {
        return Some(PauseReason::Battery);
    }
    None
}

/// The frame rate to render at, given the configured cap.
///
/// Only applies when the output is not paused outright; a battery cap and a
/// battery pause are alternatives, not a sequence.
pub fn fps_cap(base: u32, signals: &Signals, config: &PowerConfig) -> u32 {
    let mut cap = base.max(1);
    if signals.power == PowerSource::Battery && !config.pause_on_battery {
        cap = cap.min(config.battery_fps_cap.max(1));
    }
    cap
}

/// How long an environment-driven pause condition must hold before wallflux
/// acts on it.
///
/// Window trees are noisy. A fullscreen window being mapped, a focus change, a
/// client restacking itself — any of these can make an output look covered for
/// a few frames and then not. Acting on every flicker means pausing and
/// resuming mpv several times a second, which costs more than the frames it
/// saves and is visible as a stutter.
pub const PAUSE_DEBOUNCE: Duration = Duration::from_millis(400);

/// What the engine should do about a pause condition right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Adopt this pause state.
    Apply(Option<PauseReason>),
    /// A pause is being considered; look again at this time.
    Wait(Instant),
    /// Nothing to do.
    Unchanged,
}

/// Debounces transitions into a paused state.
///
/// Asymmetric on purpose: **pausing waits, resuming does not.** A pause that
/// arrives 400 ms late wastes ten frames nobody saw. A resume that arrives
/// 400 ms late is a visibly frozen wallpaper, which reads as a bug.
#[derive(Debug, Default)]
pub struct PauseGate {
    /// The pause we are waiting to confirm, and when we first saw it.
    pending: Option<(PauseReason, Instant)>,
}

impl PauseGate {
    pub fn evaluate(
        &mut self,
        wanted: Option<PauseReason>,
        current: Option<PauseReason>,
        now: Instant,
    ) -> Decision {
        // Resuming, and changes the user asked for, take effect at once.
        let Some(reason) = wanted else {
            self.pending = None;
            return if current.is_some() {
                Decision::Apply(None)
            } else {
                Decision::Unchanged
            };
        };
        if !is_environmental(reason) {
            self.pending = None;
            return if current == wanted {
                Decision::Unchanged
            } else {
                Decision::Apply(wanted)
            };
        }
        if current == wanted {
            self.pending = None;
            return Decision::Unchanged;
        }

        match self.pending {
            // A different condition than the one we were timing: restart.
            Some((pending, _)) if pending != reason => {
                self.pending = Some((reason, now));
                Decision::Wait(now + PAUSE_DEBOUNCE)
            }
            Some((_, since)) if now.duration_since(since) >= PAUSE_DEBOUNCE => {
                self.pending = None;
                Decision::Apply(wanted)
            }
            Some((_, since)) => Decision::Wait(since + PAUSE_DEBOUNCE),
            None => {
                self.pending = Some((reason, now));
                Decision::Wait(now + PAUSE_DEBOUNCE)
            }
        }
    }
}

/// True for pause reasons that come from watching the environment, and are
/// therefore subject to transient noise. A manual pause and an idle output are
/// facts, not observations.
fn is_environmental(reason: PauseReason) -> bool {
    matches!(
        reason,
        PauseReason::Fullscreen | PauseReason::Occluded | PauseReason::Battery
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PowerConfig {
        PowerConfig::default()
    }

    #[test]
    fn a_visible_wallpaper_on_mains_renders() {
        assert_eq!(pause_reason(&Signals::default(), &config()), None);
    }

    #[test]
    fn an_output_with_no_wallpaper_is_idle() {
        let signals = Signals {
            has_wallpaper: false,
            ..Default::default()
        };
        assert_eq!(pause_reason(&signals, &config()), Some(PauseReason::Idle));
    }

    #[test]
    fn occlusion_pauses_when_enabled() {
        let signals = Signals {
            visible: false,
            ..Default::default()
        };
        assert_eq!(
            pause_reason(&signals, &config()),
            Some(PauseReason::Occluded)
        );
        let relaxed = PowerConfig {
            pause_on_occlusion: false,
            ..config()
        };
        assert_eq!(pause_reason(&signals, &relaxed), None);
    }

    #[test]
    fn fullscreen_outranks_occlusion_in_the_report() {
        // Both are true for a fullscreen video; "fullscreen" is the more
        // useful thing to show the user.
        let signals = Signals {
            visible: false,
            fullscreen: true,
            ..Default::default()
        };
        assert_eq!(
            pause_reason(&signals, &config()),
            Some(PauseReason::Fullscreen)
        );
    }

    #[test]
    fn a_manual_pause_beats_the_automatic_policies() {
        let signals = Signals {
            manual: true,
            fullscreen: true,
            ..Default::default()
        };
        assert_eq!(pause_reason(&signals, &config()), Some(PauseReason::Manual));
    }

    #[test]
    fn battery_pauses_only_when_asked() {
        let signals = Signals {
            power: PowerSource::Battery,
            ..Default::default()
        };
        assert_eq!(pause_reason(&signals, &config()), None, "off by default");
        let strict = PowerConfig {
            pause_on_battery: true,
            ..config()
        };
        assert_eq!(pause_reason(&signals, &strict), Some(PauseReason::Battery));
    }

    #[test]
    fn battery_caps_the_frame_rate_instead_of_pausing() {
        let signals = Signals {
            power: PowerSource::Battery,
            ..Default::default()
        };
        assert_eq!(fps_cap(60, &signals, &config()), 15);
    }

    #[test]
    fn battery_cap_never_raises_the_rate() {
        let signals = Signals {
            power: PowerSource::Battery,
            ..Default::default()
        };
        let config = PowerConfig {
            battery_fps_cap: 30,
            ..config()
        };
        assert_eq!(fps_cap(24, &signals, &config), 24);
    }

    #[test]
    fn a_zero_cap_never_stalls_rendering() {
        // Config validation rejects 0, but a GUI could still send one over
        // IPC, and a zero divisor here would mean "never render again".
        assert_eq!(fps_cap(0, &Signals::default(), &config()), 1);
    }

    #[test]
    fn mains_ignores_the_battery_cap() {
        assert_eq!(fps_cap(60, &Signals::default(), &config()), 60);
    }
    // --- PauseGate ---------------------------------------------------------

    #[test]
    fn a_pause_waits_for_the_debounce_to_elapse() {
        let mut gate = PauseGate::default();
        let start = Instant::now();
        assert!(matches!(
            gate.evaluate(Some(PauseReason::Occluded), None, start),
            Decision::Wait(_)
        ));
        // Still inside the window: hold.
        assert!(matches!(
            gate.evaluate(
                Some(PauseReason::Occluded),
                None,
                start + Duration::from_millis(100)
            ),
            Decision::Wait(_)
        ));
        assert_eq!(
            gate.evaluate(Some(PauseReason::Occluded), None, start + PAUSE_DEBOUNCE),
            Decision::Apply(Some(PauseReason::Occluded))
        );
    }

    #[test]
    fn a_flicker_shorter_than_the_debounce_never_pauses() {
        // The bug this exists for: a window mapping made the output look
        // covered for ~200 ms, and wallflux paused and resumed mpv twice a
        // second for as long as it lasted.
        let mut gate = PauseGate::default();
        let start = Instant::now();
        gate.evaluate(Some(PauseReason::Occluded), None, start);
        assert_eq!(
            gate.evaluate(None, None, start + Duration::from_millis(200)),
            Decision::Unchanged
        );
        // The timer was dropped, so the next sighting starts a fresh window.
        assert!(matches!(
            gate.evaluate(
                Some(PauseReason::Occluded),
                None,
                start + Duration::from_millis(300)
            ),
            Decision::Wait(_)
        ));
    }

    #[test]
    fn resuming_is_immediate() {
        let mut gate = PauseGate::default();
        let now = Instant::now();
        assert_eq!(
            gate.evaluate(None, Some(PauseReason::Occluded), now),
            Decision::Apply(None)
        );
    }

    #[test]
    fn a_manual_pause_skips_the_debounce() {
        let mut gate = PauseGate::default();
        let now = Instant::now();
        assert_eq!(
            gate.evaluate(Some(PauseReason::Manual), None, now),
            Decision::Apply(Some(PauseReason::Manual))
        );
    }

    #[test]
    fn an_idle_output_skips_the_debounce() {
        let mut gate = PauseGate::default();
        let now = Instant::now();
        assert_eq!(
            gate.evaluate(Some(PauseReason::Idle), None, now),
            Decision::Apply(Some(PauseReason::Idle))
        );
    }

    #[test]
    fn switching_condition_restarts_the_timer() {
        let mut gate = PauseGate::default();
        let start = Instant::now();
        gate.evaluate(Some(PauseReason::Occluded), None, start);
        // Fullscreen replaces occlusion just before the first would fire.
        let late = start + PAUSE_DEBOUNCE - Duration::from_millis(10);
        assert!(matches!(
            gate.evaluate(Some(PauseReason::Fullscreen), None, late),
            Decision::Wait(_)
        ));
        assert!(
            matches!(
                gate.evaluate(Some(PauseReason::Fullscreen), None, start + PAUSE_DEBOUNCE),
                Decision::Wait(_)
            ),
            "the original deadline must not carry over to a new condition"
        );
        assert_eq!(
            gate.evaluate(Some(PauseReason::Fullscreen), None, late + PAUSE_DEBOUNCE),
            Decision::Apply(Some(PauseReason::Fullscreen))
        );
    }

    #[test]
    fn an_already_applied_state_is_unchanged() {
        let mut gate = PauseGate::default();
        let now = Instant::now();
        assert_eq!(
            gate.evaluate(
                Some(PauseReason::Occluded),
                Some(PauseReason::Occluded),
                now
            ),
            Decision::Unchanged
        );
    }
}
