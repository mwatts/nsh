//! The software keyboard a headless phone shows.

use nsh_core::Points;

/// Height of the software keyboard while an editable field is focused,
/// when `open` does not set one.
pub(crate) const DEFAULT_KEYBOARD: Points = Points(336.0);

/// The height `open`'s `keyboard_height` asks for, or the phone default.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SoftwareKeyboard {
    height: Option<Points>,
}

impl Default for SoftwareKeyboard {
    fn default() -> Self {
        Self { height: None }
    }
}

impl SoftwareKeyboard {
    pub(crate) fn set(&mut self, height: Points) {
        self.height = Some(height);
    }

    /// `phone` is `platform: "ios"` or `"android"`. A desktop stays at 0.
    /// A control that is focused and is not an editable field stays at 0.
    pub(crate) fn height(self, phone: bool, editable_focused: bool) -> Points {
        if phone && editable_focused {
            self.height.unwrap_or(DEFAULT_KEYBOARD)
        } else {
            Points::ZERO
        }
    }
}
