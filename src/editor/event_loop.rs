/// State for the main editor frame loop.
#[derive(Debug)]
pub struct FrameState {
    pub next_step: Option<f64>,
    pub last_frame_time: Option<f64>,
    pub run_threads_full: i64,
    pub fps: f64,
    pub blink_period: f64,
}

impl FrameState {
    /// Create a new frame state from config values.
    pub fn new(fps: f64, blink_period: f64) -> Self {
        Self {
            next_step: None,
            last_frame_time: None,
            run_threads_full: 0,
            fps,
            blink_period,
        }
    }

    /// Frame budget: max seconds per step before skipping thread work.
    pub fn frame_budget(&self) -> f64 {
        1.0 / self.fps - 0.002
    }

    /// Frame interval: target seconds between frames.
    pub fn frame_interval(&self) -> f64 {
        1.0 / self.fps
    }

    /// Decide if we should step (poll events + update + render) this iteration.
    pub fn should_step(&self, frame_start: f64, has_redraw: bool) -> bool {
        let force_draw = has_redraw
            && self.last_frame_time.is_some()
            && (frame_start - self.last_frame_time.unwrap_or(0.0)) > self.frame_interval();
        force_draw || self.next_step.is_none() || frame_start >= self.next_step.unwrap_or(0.0)
    }

    /// Update state after step completes.
    pub fn after_step(&mut self, frame_start: f64, did_redraw: bool) {
        if did_redraw {
            self.last_frame_time = Some(frame_start);
        }
        self.next_step = None;
    }

    /// Decide if we should run background threads this frame.
    pub fn should_run_threads(&self, frame_start: f64, now: f64) -> bool {
        now - frame_start < self.frame_budget()
    }

    /// Record thread scheduler result.
    pub fn after_threads(&mut self, threads_done: bool) {
        if threads_done {
            self.run_threads_full += 1;
        }
    }

    /// Calculate how long to wait for the next event/frame.
    /// Returns the wait duration in seconds.
    pub fn wait_time(
        &mut self,
        did_redraw: bool,
        did_step: bool,
        focused: bool,
        now: f64,
        blink_start: f64,
        time_to_wake: f64,
    ) -> WaitDecision {
        if did_redraw {
            self.run_threads_full = 0;
            return WaitDecision::PollThenSleep {
                max_sleep: self.post_render_sleep(now, time_to_wake),
            };
        }

        if focused || !did_step || self.run_threads_full < 2 {
            if self.next_step.is_none() {
                let cursor_time = self.cursor_blink_time(now, blink_start);
                self.next_step = Some(now + cursor_time);
            }
            let wait = (self.next_step.unwrap_or(0.0) - now).min(time_to_wake);
            WaitDecision::Wait(wait)
        } else {
            WaitDecision::WaitIndefinitely
        }
    }

    /// Calculate time until next cursor blink transition.
    fn cursor_blink_time(&self, now: f64, blink_start: f64) -> f64 {
        let t = now - blink_start;
        let h = self.blink_period / 2.0;
        let dt = (t / h).ceil() * h - t;
        dt + self.frame_interval()
    }

    /// Calculate post-render sleep time.
    fn post_render_sleep(&self, now: f64, time_to_wake: f64) -> f64 {
        let frame_start = self.last_frame_time.unwrap_or(now);
        let elapsed = now - frame_start;
        let next_frame = (self.frame_interval() - elapsed).max(0.0);
        if self.next_step.is_none() {
            self.next_step.unwrap_or(0.0); // no-op read
        }
        next_frame.min(time_to_wake)
    }

    /// Reset step timing after receiving an event.
    pub fn on_event_received(&mut self) {
        self.next_step = None;
    }
}

/// Decision from wait_time calculation.
#[derive(Debug, PartialEq)]
pub enum WaitDecision {
    /// Wait for the given duration, then proceed.
    Wait(f64),
    /// Poll for pending events, then sleep up to max_sleep if none.
    PollThenSleep { max_sleep: f64 },
    /// Wait indefinitely for an event (unfocused, idle).
    WaitIndefinitely,
}

/// Classify how an event should be routed in the step function.
#[derive(Debug, PartialEq, Eq)]
pub enum EventRouting {
    /// Skip this event (e.g. textinput after keymap consumed the key).
    Skip,
    /// Route through try(on_event, ...) without checking keymap result.
    Dispatch,
    /// Route through try(on_event, ...) and track keymap result.
    DispatchTrackKeymap,
    /// Set redraw and break the event poll loop.
    RedrawAndBreak,
}

/// Decide how to route an event based on its type and whether the previous
/// keypressed was consumed by the keymap.
pub fn classify_event(event_type: &str, did_keymap: bool) -> EventRouting {
    match event_type {
        "textinput" if did_keymap => EventRouting::Skip,
        "mousemoved" => EventRouting::Dispatch,
        "enteringforeground" => EventRouting::RedrawAndBreak,
        "keypressed" => EventRouting::DispatchTrackKeymap,
        _ => EventRouting::DispatchTrackKeymap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_state_should_step_first_frame() {
        let state = FrameState::new(60.0, 0.8);
        assert!(state.should_step(0.0, false));
    }

    #[test]
    fn frame_state_should_step_forced_redraw() {
        let mut state = FrameState::new(60.0, 0.8);
        state.last_frame_time = Some(0.0);
        // 0.1s > 1/60 ≈ 0.016, so force redraw
        assert!(state.should_step(0.1, true));
    }

    #[test]
    fn frame_state_should_not_step_too_soon() {
        let mut state = FrameState::new(60.0, 0.8);
        state.next_step = Some(1.0);
        assert!(!state.should_step(0.5, false));
    }

    #[test]
    fn frame_budget_positive() {
        let state = FrameState::new(60.0, 0.8);
        assert!(state.frame_budget() > 0.0);
        assert!(state.frame_budget() < state.frame_interval());
    }

    #[test]
    fn classify_event_textinput_after_keymap() {
        assert_eq!(classify_event("textinput", true), EventRouting::Skip);
        assert_eq!(
            classify_event("textinput", false),
            EventRouting::DispatchTrackKeymap
        );
    }

    #[test]
    fn classify_event_mousemoved() {
        assert_eq!(classify_event("mousemoved", false), EventRouting::Dispatch);
    }

    #[test]
    fn classify_event_foreground() {
        assert_eq!(
            classify_event("enteringforeground", false),
            EventRouting::RedrawAndBreak
        );
    }

    #[test]
    fn classify_event_keypressed() {
        assert_eq!(
            classify_event("keypressed", false),
            EventRouting::DispatchTrackKeymap
        );
    }

    #[test]
    fn wait_decision_unfocused_idle() {
        let mut state = FrameState::new(60.0, 0.8);
        state.run_threads_full = 5;
        let decision = state.wait_time(false, true, false, 1.0, 0.0, 10.0);
        assert_eq!(decision, WaitDecision::WaitIndefinitely);
    }

    #[test]
    fn wait_decision_after_redraw() {
        let mut state = FrameState::new(60.0, 0.8);
        state.last_frame_time = Some(1.0);
        let decision = state.wait_time(true, true, true, 1.001, 0.0, 10.0);
        assert!(matches!(decision, WaitDecision::PollThenSleep { .. }));
    }

    #[test]
    fn on_event_resets_step() {
        let mut state = FrameState::new(60.0, 0.8);
        state.next_step = Some(5.0);
        state.on_event_received();
        assert!(state.next_step.is_none());
    }
}
