//! The step `keyboard.press(<keys>)`: a key with its modifiers, as a
//! hardware keyboard gives it, on every platform (`open`'s `platform`: the
//! phones and tablets have one too, a tablet's keyboard, a Chromebook's).
//! The app's shortcuts (`nsh/keymap`) take it first, as the platforms'
//! keymaps do: the newest binding of the keystroke that fires where the
//! focus is reports `onKeyCommand`. Else Tab moves the keyboard focus
//! between the inputs and the views that are its stops (those that take
//! presses, and `focusable` ones that take none) where the platform
//! does (macOS, the web), Return and Space press the pressable it is on,
//! the focused input takes the keys that edit it, and Escape goes back.

use nsh_core::{
    Command, EventKind, EventPayload, MountBatch, MountItem, NodeId, PayloadValue, ROOT_ID,
    keymap::{Key, KeyMatch, Keystroke},
    platform::Os,
    tailwind::state::FOCUSED,
};

use super::Headless;

impl Headless {
    /// The steps `keyboard.down(<keys>)` / `keyboard.up(<keys>)`: holds or
    /// lets go of modifier keys (`"mod"`, `"shift"`, `"alt"`, `"ctrl"`,
    /// `"meta"`, joined by `+`), which the presses, drags and hovers until
    /// then carry, as a hand on the keyboard does while the pointer acts.
    pub fn hold_keys(&mut self, keys: &str, down: bool) -> Result<(), String> {
        self.step(format!(
            "keyboard.{}({keys})",
            if down { "down" } else { "up" }
        ));
        let apple = matches!(self.environment.os, Os::MacOs | Os::Ios);
        for name in keys.split('+') {
            let flag = match name.trim().to_ascii_lowercase().as_str() {
                "mod" if apple => &mut self.held.cmd,
                "mod" | "ctrl" | "control" => &mut self.held.ctrl,
                "alt" | "option" => &mut self.held.alt,
                "shift" => &mut self.held.shift,
                "meta" | "cmd" | "command" => &mut self.held.cmd,
                other => {
                    return Err(format!(
                        "`{other}` is no modifier key: mod, shift, alt, ctrl or meta"
                    ));
                }
            };
            *flag = down;
        }
        self.done()
    }

    /// The step `keyboard.press(<keys>)`: presses `keys`, a keystroke as
    /// `nsh/keymap` names it (`"mod+k"`, `"shift+tab"`, `"left"`,
    /// `"escape"`; `mod` is the app's platform's command key). A binding of
    /// the app's keymap that fires where the focus is hears it
    /// (`onKeyCommand`) and takes the key unless it passes it through. Else
    /// Tab moves the keyboard focus between the inputs and the views that
    /// are its stops where the platform does (macOS, the web, a terminal),
    /// Return and Space press the pressable it is on, and the focused input
    /// reports the arrows, Delete, Escape and Tab (`onKeyPress`, with the
    /// modifiers) and then acts: Left and Right move the caret a character
    /// (Shift extends the selection), Home and End to the text's edges,
    /// Backspace and Delete delete (reported as `Backspace`), Enter is the
    /// return key, `mod+a` selects all, and a character key types (Shift
    /// as on the US layout). A character typed after a handled key, while
    /// no editable field has the keyboard, waits for the next one. Arrows,
    /// Delete, Backspace, and Enter still reach a focused field that is
    /// not editable. Escape drops that wait, and with nothing focused goes
    /// back as the desktops' back key does when there is something to go
    /// back from.
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
        // A hardware keyboard fires the app's shortcuts (`on_key`,
        // `keymap.bind`), on every platform.
        let found = match self.screen.keymap() {
            Some(keymap) => keymap.find(stroke, focused.is_some(), false),
            None => KeyMatch::None,
        };
        // A plain character waits for an editable field only when nothing
        // else takes it. A focused input that cannot be edited holds the
        // character (it is not typed there). A binding (typeahead) and
        // Space or Enter on a focused pressable still run: the wait is for
        // letters typed while focus moves to a field, not for those keys.
        let plain_mods = !(m.ctrl || m.alt || m.cmd);
        let activates_pressable = plain_mods
            && !m.shift
            && matches!(stroke.key, Key::Enter | Key::Char(' '))
            && self.screen.focused().is_none()
            && self
                .screen
                .key_focus()
                .is_some_and(|view| self.screen.hears(view, EventKind::Press));
        let input_focused = self
            .screen
            .focused()
            .is_some_and(|id| self.screen.input(id).is_some());
        if let Key::Char(c) = stroke.key
            && plain_mods
            && self.editable_focus().is_none()
            && (input_focused || (!activates_pressable && matches!(found, KeyMatch::None)))
        {
            let c = if m.shift { shifted(c) } else { c };
            if self
                .hold
                .offer(self.clock.now(), &c.to_string(), false, false)
            {
                return self.done();
            }
        }
        if let Some(id) = found.fired() {
            self.engine.control().dispatch_event(
                ROOT_ID,
                EventKind::KeyCommand,
                EventPayload::Value(PayloadValue::Number(f64::from(id))),
            );
            if stroke.key != Key::Escape {
                self.arm_text();
            }
        }
        // A binding passing the key through leaves it to what follows, as a
        // platform does: on the screen as it is when the key comes.
        if found.consumed() {
            return self.done();
        }
        let plain = !(m.ctrl || m.alt || m.cmd);
        if stroke.key == Key::Tab && plain && self.tabs_to_views() {
            self.tab(m.shift);
            return self.done();
        }
        // A stop that takes no presses (a `focusable` chart) hears its keys
        // through the keymap only, above.
        if let Some(view) = self.screen.key_focus()
            && self.screen.focused().is_none()
            && self.screen.hears(view, EventKind::Press)
            && plain
            && !m.shift
            && matches!(stroke.key, Key::Enter | Key::Char(' '))
        {
            for kind in [EventKind::PressDown, EventKind::PressUp, EventKind::Press] {
                self.send(view, kind, EventPayload::None);
            }
            return self.done();
        }
        // A plain character waits for an editable field. Every other key
        // still reaches the focused field, including one that is read-only.
        let plain_char = matches!(stroke.key, Key::Char(_)) && plain;
        if let Some(input) = focused.filter(|&id| {
            !plain_char
                || self
                    .screen
                    .input(id)
                    .is_some_and(|input| input.config.editable)
        }) {
            self.input_key(input, stroke, apple);
            return self.done();
        }
        if let Key::Char(c) = stroke.key
            && !(m.ctrl || m.alt || m.cmd)
        {
            let c = if m.shift { shifted(c) } else { c };
            if self
                .hold
                .offer(self.clock.now(), &c.to_string(), false, false)
            {
                return self.done();
            }
        }
        if stroke.key == Key::Escape
            && !(m.ctrl || m.alt || m.cmd || m.shift)
            && self.screen.back_target() != crate::screen::Back::Nothing
        {
            return self.back();
        }
        self.done()
    }

    /// Whether Tab stops at views (not only inputs), as on macOS, the web
    /// and in a terminal (the other platforms move it between fields only).
    fn tabs_to_views(&self) -> bool {
        matches!(self.environment.os, Os::MacOs | Os::Web)
            || self.environment.idiom == nsh_core::platform::Idiom::Terminal
    }

    /// Tab (`back`: Shift+Tab): the keyboard focus moves to the next stop
    /// ([`crate::screen::Screen::tab_stops`]), around from the last.
    fn tab(&mut self, back: bool) {
        let stops = self.screen.tab_stops();
        let current = self.screen.focused().or(self.screen.key_focus());
        let at = current.and_then(|id| stops.iter().position(|s| *s == id));
        let Some(next) = nsh_core::input::tab_stop(at, stops.len(), back, true) else {
            return;
        };
        self.move_focus(Some(stops[next]));
    }

    /// Moves the keyboard focus to `to` (an input or a view), telling
    /// the view it leaves and the one it reaches.
    pub(super) fn move_focus(&mut self, to: Option<NodeId>) {
        let from = self.screen.focused().or(self.screen.key_focus());
        if from == to {
            return;
        }
        if let Some(old) = from {
            self.screen.set_focused(None);
            self.screen.set_key_focus(None);
            self.set_focused_state(old, false);
            self.send(old, EventKind::Blur, EventPayload::None);
        }
        if let Some(new) = to {
            if self.screen.input(new).is_some() {
                self.screen.set_focused(Some(new));
            } else {
                self.screen.set_key_focus(Some(new));
            }
            self.set_focused_state(new, true);
            self.send(new, EventKind::Focus, EventPayload::None);
        }
    }

    /// The app moved the focus (`focus()`, `blur()`) in `batch`, applied
    /// with input `focused` focused before it: platforms tell the views as
    /// when the user moves it. The screen applied the calls on inputs;
    /// where Tab stops at views that take presses, a call on one of those
    /// moves the keyboard focus as Tab does (the batch's last call counts).
    pub(super) fn focus_calls(&mut self, batch: &MountBatch, focused: Option<NodeId>) {
        let now = self.screen.focused();
        if now != focused {
            if let Some(old) = focused {
                self.set_focused_state(old, false);
                self.send(old, EventKind::Blur, EventPayload::None);
            }
            if let Some(new) = now {
                // The pressable that had the keyboard focus loses it.
                if let Some(key) = self.screen.key_focus() {
                    self.screen.set_key_focus(None);
                    self.set_focused_state(key, false);
                    self.send(key, EventKind::Blur, EventPayload::None);
                }
                self.set_focused_state(new, true);
                self.send(new, EventKind::Focus, EventPayload::None);
            }
        }
        if !self.tabs_to_views() {
            return;
        }
        let mut key = self.screen.key_focus();
        let mut to = None;
        for item in &batch.items {
            let MountItem::Command { id, command } = item else {
                continue;
            };
            let id = *id;
            match command {
                Command::Focus if self.screen.input(id).is_some() => {
                    to = None;
                    key = None;
                }
                Command::Focus if id != ROOT_ID && self.screen.tab_stops().contains(&id) => {
                    to = Some(Some(id));
                    key = Some(id);
                }
                Command::Blur if id != ROOT_ID && key == Some(id) => {
                    to = Some(None);
                    key = None;
                }
                _ => {}
            }
        }
        if let Some(to) = to {
            self.move_focus(to);
        }
    }

    /// A view's or an input's `focus:` state, which follows the keyboard
    /// focus: reported whatever the view tracks, as the platforms report
    /// it (`nsh_core::input::reported_states`; the core works out
    /// `focus-within` from it).
    pub(super) fn set_focused_state(&mut self, id: NodeId, on: bool) {
        if let Some(states) = self.screen.set_state(id, FOCUSED, on) {
            self.engine.control().set_states(id, states);
        }
    }

    /// A handled key just moved where typing goes. Characters that follow,
    /// before an editable field has the keyboard, wait three seconds. A test
    /// that dispatches the command itself calls this too.
    pub fn arm_text(&mut self) {
        self.hold.arm(self.clock.now());
    }

    /// The window went inactive. Characters that were waiting are dropped.
    pub fn deactivate(&mut self) {
        self.hold.disarm();
    }

    /// Drops text that was waiting for a field.
    fn disarm_text(&mut self) {
        self.hold.disarm();
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
        if self.editable_focus().is_none() {
            return;
        }
        let text = self.hold.take(self.clock.now());
        if text.is_empty() {
            return;
        }
        let Some(id) = self.editable_focus() else {
            return;
        };
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
