use std::collections::HashMap;

use crate::input::action::CoreAction;

const MAX_PLAYBACK_DEPTH: usize = 1000;

pub struct MacroRecorder {
    registers: HashMap<char, Vec<CoreAction>>,
    /// Registers in the order they were first recorded into.
    order: Vec<char>,
    recording: Option<(char, Vec<CoreAction>)>,
    last_played: Option<char>,
    playback_depth: usize,
}

impl Default for MacroRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl MacroRecorder {
    pub fn new() -> Self {
        Self {
            registers: HashMap::new(),
            order: Vec::new(),
            recording: None,
            last_played: None,
            playback_depth: 0,
        }
    }

    pub fn start_recording(&mut self, register: char) {
        self.recording = Some((register, Vec::new()));
    }

    pub fn stop_recording(&mut self) {
        if let Some((reg, actions)) = self.recording.take() {
            if !self.registers.contains_key(&reg) {
                self.order.push(reg);
            }
            self.registers.insert(reg, actions);
            // Treat the freshly recorded register as the "last" macro so it can
            // be replayed immediately without having been played first.
            self.last_played = Some(reg);
        }
    }

    /// Registers that have a recorded macro, in the order they were first
    /// recorded into.
    pub fn registered(&self) -> &[char] {
        &self.order
    }

    pub fn record(&mut self, action: &CoreAction) {
        if let Some((_, ref mut actions)) = self.recording {
            actions.push(action.clone());
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn recording_register(&self) -> Option<char> {
        self.recording.as_ref().map(|(r, _)| *r)
    }

    pub fn get(&self, register: char) -> Option<&Vec<CoreAction>> {
        self.registers.get(&register)
    }

    pub fn last_played(&self) -> Option<char> {
        self.last_played
    }

    pub fn set_last_played(&mut self, register: char) {
        self.last_played = Some(register);
    }

    pub fn enter_playback(&mut self) -> bool {
        if self.playback_depth >= MAX_PLAYBACK_DEPTH {
            return false;
        }
        self.playback_depth += 1;
        true
    }

    pub fn exit_playback(&mut self) {
        self.playback_depth = self.playback_depth.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_and_stop_recording() {
        let mut rec = MacroRecorder::new();
        assert!(!rec.is_recording());

        rec.start_recording('a');
        assert!(rec.is_recording());
        assert_eq!(rec.recording_register(), Some('a'));

        rec.record(&CoreAction::MoveRight);
        rec.record(&CoreAction::MoveDown);
        rec.stop_recording();

        assert!(!rec.is_recording());
        let actions = rec.get('a').unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0], CoreAction::MoveRight);
        assert_eq!(actions[1], CoreAction::MoveDown);
    }

    #[test]
    fn overwrite_register() {
        let mut rec = MacroRecorder::new();
        rec.start_recording('a');
        rec.record(&CoreAction::MoveRight);
        rec.stop_recording();

        rec.start_recording('a');
        rec.record(&CoreAction::MoveLeft);
        rec.stop_recording();

        let actions = rec.get('a').unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], CoreAction::MoveLeft);
    }

    #[test]
    fn empty_register_returns_none() {
        let rec = MacroRecorder::new();
        assert!(rec.get('z').is_none());
    }

    #[test]
    fn stop_recording_marks_register_as_last_played() {
        let mut rec = MacroRecorder::new();
        rec.start_recording('a');
        rec.record(&CoreAction::MoveRight);
        rec.stop_recording();
        // A freshly recorded macro can be replayed via "play last".
        assert_eq!(rec.last_played(), Some('a'));
    }

    #[test]
    fn last_played_tracking() {
        let mut rec = MacroRecorder::new();
        assert!(rec.last_played().is_none());
        rec.set_last_played('b');
        assert_eq!(rec.last_played(), Some('b'));
    }

    #[test]
    fn playback_depth_guard() {
        let mut rec = MacroRecorder::new();
        for _ in 0..1000 {
            assert!(rec.enter_playback());
        }
        // 1001st should fail
        assert!(!rec.enter_playback());

        rec.exit_playback();
        assert!(rec.enter_playback());
    }

    #[test]
    fn registered_tracks_insertion_order() {
        let mut rec = MacroRecorder::new();
        assert!(rec.registered().is_empty());

        for reg in ['c', 'a', 'b'] {
            rec.start_recording(reg);
            rec.record(&CoreAction::MoveRight);
            rec.stop_recording();
        }
        assert_eq!(rec.registered(), &['c', 'a', 'b']);

        // Overwriting an existing register keeps its original position.
        rec.start_recording('a');
        rec.record(&CoreAction::MoveLeft);
        rec.stop_recording();
        assert_eq!(rec.registered(), &['c', 'a', 'b']);
    }

    #[test]
    fn record_skips_when_not_recording() {
        let mut rec = MacroRecorder::new();
        rec.record(&CoreAction::MoveRight); // should be a no-op
        assert!(rec.get('a').is_none());
    }
}
