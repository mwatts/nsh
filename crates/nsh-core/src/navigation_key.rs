//! Navigation keys a hardware keyboard sends to a focused field. The name
//! is what `onKeyPress` reports. The field still moves the caret or deletes.

/// A key that moves the caret, deletes forward, or tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationKey {
    /// Left arrow.
    ArrowLeft,
    /// Right arrow.
    ArrowRight,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Forward delete.
    Delete,
    /// Tab.
    Tab,
    /// Home.
    Home,
    /// End.
    End,
}

impl NavigationKey {
    /// The `onKeyPress` name, the same names the GPUI backend sends.
    pub fn press_name(self) -> &'static str {
        match self {
            Self::ArrowLeft => "ArrowLeft",
            Self::ArrowRight => "ArrowRight",
            Self::ArrowUp => "ArrowUp",
            Self::ArrowDown => "ArrowDown",
            Self::Delete => "Delete",
            Self::Tab => "Tab",
            Self::Home => "Home",
            Self::End => "End",
        }
    }

    /// An Android `KeyEvent` key code, or nothing for a key this set does
    /// not name. Backspace (`KEYCODE_DEL`, 67) stays a separate key.
    pub fn from_android_keycode(code: i32) -> Option<Self> {
        Some(match code {
            19 => Self::ArrowUp,
            20 => Self::ArrowDown,
            21 => Self::ArrowLeft,
            22 => Self::ArrowRight,
            61 => Self::Tab,
            112 => Self::Delete,
            122 => Self::Home,
            123 => Self::End,
            _ => return None,
        })
    }

    /// One UIKit function-key character (`UIKey.charactersIgnoringModifiers`),
    /// or Tab. Nothing for a character this set does not name.
    pub fn from_uikit_character(character: char) -> Option<Self> {
        Some(match character {
            '\u{F700}' => Self::ArrowUp,
            '\u{F701}' => Self::ArrowDown,
            '\u{F702}' => Self::ArrowLeft,
            '\u{F703}' => Self::ArrowRight,
            '\u{F728}' => Self::Delete,
            '\t' => Self::Tab,
            '\u{F729}' => Self::Home,
            '\u{F72B}' => Self::End,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_keycodes_and_uikit_characters_use_the_press_names() {
        let keys = [
            (19, '\u{F700}', "ArrowUp"),
            (20, '\u{F701}', "ArrowDown"),
            (21, '\u{F702}', "ArrowLeft"),
            (22, '\u{F703}', "ArrowRight"),
            (112, '\u{F728}', "Delete"),
            (61, '\t', "Tab"),
            (122, '\u{F729}', "Home"),
            (123, '\u{F72B}', "End"),
        ];
        for (code, character, name) in keys {
            assert_eq!(
                NavigationKey::from_android_keycode(code).map(NavigationKey::press_name),
                Some(name)
            );
            assert_eq!(
                NavigationKey::from_uikit_character(character).map(NavigationKey::press_name),
                Some(name)
            );
        }
        assert_eq!(NavigationKey::from_android_keycode(67), None);
        assert_eq!(NavigationKey::from_uikit_character('a'), None);
        assert_eq!(NavigationKey::from_uikit_character('\u{1b}'), None);
    }
}
