//! Composer state + key event → command pipeline.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::command::{Command, parse};

#[derive(Default)]
pub struct Composer {
    pub buffer: String,
    pub cursor: usize,
}

impl Composer {
    pub fn cursor_col(&self) -> u16 {
        // ASCII-only assumption: byte length == column count. Wider
        // chars would need a unicode-width pass; out of v1 scope.
        self.buffer.len().min(self.cursor) as u16
    }
}

pub enum KeyOutcome {
    Nothing,
    Submit(Command),
    Quit,
    Redraw,
}

pub fn handle_key(composer: &mut Composer, key: KeyEvent) -> KeyOutcome {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char('c') = key.code {
            return KeyOutcome::Quit;
        }
    }
    match key.code {
        KeyCode::Enter => {
            let line = std::mem::take(&mut composer.buffer);
            composer.cursor = 0;
            KeyOutcome::Submit(parse(&line))
        }
        KeyCode::Char(c) => {
            composer.buffer.push(c);
            composer.cursor = composer.buffer.len();
            KeyOutcome::Redraw
        }
        KeyCode::Backspace => {
            composer.buffer.pop();
            composer.cursor = composer.buffer.len();
            KeyOutcome::Redraw
        }
        KeyCode::Esc => {
            composer.buffer.clear();
            composer.cursor = 0;
            KeyOutcome::Redraw
        }
        _ => KeyOutcome::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn k(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty())
    }

    #[test]
    fn typing_appends_to_buffer() {
        let mut c = Composer::default();
        for ch in "hi".chars() {
            handle_key(&mut c, k(ch));
        }
        assert_eq!(c.buffer, "hi");
    }

    #[test]
    fn enter_submits_parsed_command() {
        let mut c = Composer::default();
        for ch in "/help".chars() {
            handle_key(&mut c, k(ch));
        }
        let out = handle_key(&mut c, KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(matches!(out, KeyOutcome::Submit(Command::Help)));
        assert!(c.buffer.is_empty());
    }

    #[test]
    fn ctrl_c_quits() {
        let mut c = Composer::default();
        let out = handle_key(
            &mut c,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(matches!(out, KeyOutcome::Quit));
    }

    #[test]
    fn backspace_pops() {
        let mut c = Composer::default();
        for ch in "abc".chars() {
            handle_key(&mut c, k(ch));
        }
        handle_key(
            &mut c,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()),
        );
        assert_eq!(c.buffer, "ab");
    }

    #[test]
    fn esc_clears() {
        let mut c = Composer::default();
        for ch in "abc".chars() {
            handle_key(&mut c, k(ch));
        }
        handle_key(&mut c, KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(c.buffer.is_empty());
    }
}
