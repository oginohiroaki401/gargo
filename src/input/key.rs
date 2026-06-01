//! Target-agnostic key event types.
//!
//! On native builds these are re-exports of `crossterm`'s event types, so the
//! terminal input path and the existing `keymap` logic/tests compile unchanged.
//!
//! On `wasm32` (the browser editor) `crossterm` is not available, so we provide
//! a minimal mirror that exposes the exact API surface `keymap` relies on
//! (`KeyCode` variants, `KeyModifiers` bitflags, and `KeyEvent { code, modifiers }`).
//! The browser front-end synthesizes these from DOM `KeyboardEvent`s.

#[cfg(not(target_arch = "wasm32"))]
pub use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[cfg(target_arch = "wasm32")]
pub use wasm_keys::{KeyCode, KeyEvent, KeyModifiers};

#[cfg(target_arch = "wasm32")]
mod wasm_keys {
    /// Mirror of the `crossterm::event::KeyCode` variants used by `keymap`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum KeyCode {
        Char(char),
        F(u8),
        Esc,
        Enter,
        Backspace,
        Delete,
        Tab,
        BackTab,
        Left,
        Right,
        Up,
        Down,
        Home,
        End,
        PageUp,
        PageDown,
        Insert,
        Null,
    }

    /// Mirror of `crossterm::event::KeyModifiers` exposing just the bitflag
    /// surface `keymap` uses (`NONE`/`CONTROL`/`SHIFT`/`ALT`, `contains`, `|`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KeyModifiers(u8);

    impl KeyModifiers {
        pub const NONE: KeyModifiers = KeyModifiers(0);
        pub const SHIFT: KeyModifiers = KeyModifiers(0b0000_0001);
        pub const CONTROL: KeyModifiers = KeyModifiers(0b0000_0010);
        pub const ALT: KeyModifiers = KeyModifiers(0b0000_0100);

        pub const fn empty() -> KeyModifiers {
            KeyModifiers(0)
        }

        pub const fn contains(self, other: KeyModifiers) -> bool {
            (self.0 & other.0) == other.0
        }
    }

    impl std::ops::BitOr for KeyModifiers {
        type Output = KeyModifiers;
        fn bitor(self, rhs: KeyModifiers) -> KeyModifiers {
            KeyModifiers(self.0 | rhs.0)
        }
    }

    impl std::ops::BitOrAssign for KeyModifiers {
        fn bitor_assign(&mut self, rhs: KeyModifiers) {
            self.0 |= rhs.0;
        }
    }

    impl Default for KeyModifiers {
        fn default() -> Self {
            KeyModifiers::NONE
        }
    }

    /// Mirror of `crossterm::event::KeyEvent` (only `code`/`modifiers` are read
    /// by `keymap`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KeyEvent {
        pub code: KeyCode,
        pub modifiers: KeyModifiers,
    }

    impl KeyEvent {
        pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
            KeyEvent { code, modifiers }
        }
    }
}
