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
    /// as on the US layout). With no input focused, Escape goes back as
    /// the desktops' back key does when there is something to go back
    /// from.
    pub fn press_key(&mut self, keys: &str) -> Result<(), String> {
        self.step(format!("keyboard.press({keys})"));
        let apple =
            nsh_core::keymap::apple_keys(self.environment.os, self.environment.apple_keyboard);
        let stroke = Keystroke::parse_with(keys, apple).map_err(|e| e.to_string())?;
        let focused = self
            .screen
            .focused()
            .filter(|&id| self.screen.input(id).is_some());
        // Desktop platforms fire the app's shortcuts (`on_key`, `keymap.bind`).
        let found = match self.screen.keymap() {
            Some(keymap) if self.environment.desktop() => {
                keymap.find(stroke, focused.is_some(), false)
            }
            _ => KeyMatch::None,
        };
        if let KeyMatch::Fire(id) = found {
            self.engine.control().dispatch_event(
                ROOT_ID,
                EventKind::KeyCommand,
                EventPayload::Value(PayloadValue::Number(f64::from(id))),
            );
        }
        if found.consumed() {
            return self.done();
        }
        if let Some(input) = focused {
            self.input_key(input, stroke, apple);
            return self.done();
        }
        let m = stroke.modifiers;
        if stroke.key == Key::Escape
            && !(m.ctrl || m.alt || m.cmd || m.shift)
            && self.screen.back_target() != crate::screen::Back::Nothing
        {
            return self.back();
        }
        self.done()
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
