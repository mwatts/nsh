//! The step `keyboard.press(<keys>)`: a key with its modifiers, as a
//! hardware keyboard gives it. On a desktop platform (`open`'s
//! `platform`) the app's shortcuts (`nsh/keymap`) take it first, as the
//! platforms' keymaps do: the newest binding of the
//! keystroke that fires where the focus is reports `onKeyCommand`. Else
//! the focused input takes the keys that edit it, and Escape goes back.

use nsh_core::{
    EventKind, EventPayload, PayloadValue, ROOT_ID,
    keymap::{Key, KeyMatch, Keystroke},
};

use super::Headless;

impl Headless {
    /// The step `keyboard.press(<keys>)`: presses `keys`, a keystroke as
    /// `nsh/keymap` names it (`"mod+k"`, `"shift+tab"`, `"left"`,
    /// `"escape"`; `mod` is the app's platform's command key). A binding of
    /// the app's keymap that fires where the focus is hears it
    /// (`onKeyCommand`) and takes the key. Else the focused input reports
    /// the arrows, Delete, Escape and Tab (`onKeyPress`, with the
    /// modifiers) and then acts: Left and Right move the caret a character
    /// (Shift extends the selection), Home and End to the text's edges,
    /// Backspace and Delete delete (reported as `Backspace`), Enter is the
    /// return key, `mod+a` selects all, and a character key types (Shift
    /// as on the US layout). A character typed after a handled key, while
    /// no editable field has the keyboard, waits for the next one. Escape
    /// drops that wait, and with nothing focused goes back as the
    /// desktops' back key does when there is something to go back from.
    pub fn press_key(&mut self, keys: &str) -> Result<(), String> {
        self.step(format!("keyboard.press({keys})"));
        self.deliver_held();
        let apple =
            nsh_core::keymap::apple_keys(self.environment.os, self.environment.apple_keyboard);
        let stroke = Keystroke::parse_with(keys, apple).map_err(|e| e.to_string())?;
        let focused = self
            .screen
            .focused()
            .filter(|&id| self.screen.input(id).is_some());
        let m = stroke.modifiers;
        if stroke.key == Key::Escape && !(m.ctrl || m.alt || m.cmd || m.shift) {
            self.disarm_text();
        }
        // Desktop platforms fire the app's shortcuts (`on_key`, `keymap.bind`).
        let found = match self.screen.keymap() {
            Some(keymap) if self.environment.desktop() => {
                keymap.find(stroke, focused.is_some(), false)
            }
            _ => KeyMatch::None,
        };
        if let Key::Char(c) = stroke.key
            && !(m.ctrl || m.alt || m.cmd)
            && self.editable_focus().is_none()
            && self.text_armed()
        {
            let c = if m.shift { shifted(c) } else { c };
            self.held.push(c);
            return self.done();
        }
        if let KeyMatch::Fire(id) = found {
            self.engine.control().dispatch_event(
                ROOT_ID,
                EventKind::KeyCommand,
                EventPayload::Value(PayloadValue::Number(f64::from(id))),
            );
            if stroke.key != Key::Escape {
                self.arm_text();
            }
        }
        if found.consumed() {
            return self.done();
        }
        if let Some(input) = focused.filter(|&id| {
            self.screen
                .input(id)
                .is_some_and(|input| input.config.editable)
        }) {
            self.input_key(input, stroke, apple);
            return self.done();
        }
        if let Key::Char(c) = stroke.key
            && !(m.ctrl || m.alt || m.cmd)
            && self.text_armed()
        {
            let c = if m.shift { shifted(c) } else { c };
            self.held.push(c);
            return self.done();
        }
        if stroke.key == Key::Escape
            && !(m.ctrl || m.alt || m.cmd || m.shift)
            && self.screen.back_target() != crate::screen::Back::Nothing
        {
            return self.back();
        }
        self.done()
    }

    /// A handled key just moved where typing goes. Characters that follow,
    /// before an editable field has the keyboard, wait three seconds. A test
    /// that dispatches the command itself calls this too.
    pub fn arm_text(&mut self) {
        self.held_until = Some(self.clock.now() + std::time::Duration::from_secs(3));
    }

    /// Drops text that was waiting for a field.
    fn disarm_text(&mut self) {
        self.held.clear();
        self.held_until = None;
    }

    /// Whether text typed now waits for a field.
    fn text_armed(&mut self) -> bool {
        match self.held_until {
            Some(until) if self.clock.now() < until => true,
            Some(_) => {
                self.disarm_text();
                false
            }
            None => false,
        }
    }

    /// The editable field that has the keyboard, if one does.
    fn editable_focus(&self) -> Option<nsh_core::NodeId> {
        let id = self.screen.focused()?;
        self.screen
            .input(id)
            .is_some_and(|input| input.config.editable)
            .then_some(id)
    }

    /// An editable field took the keyboard: the characters that waited go
    /// in at its caret.
    pub(super) fn deliver_held(&mut self) {
        if self.held.is_empty() || !self.text_armed() {
            return;
        }
        let Some(id) = self.editable_focus() else {
            return;
        };
        let text = std::mem::take(&mut self.held);
        self.held_until = None;
        let caret = self.screen.input(id).map_or(0, |input| input.caret);
        self.edit(id, caret, caret, &text);
    }
}

/// The character the key typing `c` types with Shift on the US layout.
pub(super) fn shifted(c: char) -> char {
    const SHIFTED: &[(char, char)] = &[
        ('`', '~'),
        ('1', '!'),
        ('2', '@'),
        ('3', '#'),
        ('4', '$'),
        ('5', '%'),
        ('6', '^'),
        ('7', '&'),
        ('8', '*'),
        ('9', '('),
        ('0', ')'),
        ('-', '_'),
        ('=', '+'),
        ('[', '{'),
        (']', '}'),
        ('\\', '|'),
        (';', ':'),
        ('\'', '"'),
        (',', '<'),
        ('.', '>'),
        ('/', '?'),
    ];
    SHIFTED
        .iter()
        .find(|(plain, _)| *plain == c)
        .map_or(c.to_ascii_uppercase(), |(_, s)| *s)
}
