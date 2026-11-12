//! The keyboard shortcuts, which can be changed on the Keybinds page.

use eframe::egui::{InputState, Key, Modifiers};

use crate::platform;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    OpenFolder,
    Settings,
    SelectAll,
    OpenPhoto,
    Pick,
    PlayPause,
    SkipBack,
    SkipForward,
    FullScreen,
    QuickExport,
    Delete,
    Back,
    Previous,
    Next,
    First,
    Last,
}

impl Action {
    pub const ALL: [Action; 16] = [
        Action::OpenFolder,
        Action::Settings,
        Action::SelectAll,
        Action::OpenPhoto,
        Action::Pick,
        Action::PlayPause,
        Action::SkipBack,
        Action::SkipForward,
        Action::FullScreen,
        Action::QuickExport,
        Action::Delete,
        Action::Back,
        Action::Previous,
        Action::Next,
        Action::First,
        Action::Last,
    ];

    /// Its name in the settings file.
    fn key(self) -> &'static str {
        match self {
            Action::OpenFolder => "open",
            Action::Settings => "settings",
            Action::SelectAll => "select_all",
            Action::OpenPhoto => "open_photo",
            Action::Pick => "pick",
            Action::PlayPause => "play_pause",
            Action::SkipBack => "skip_back",
            Action::SkipForward => "skip_forward",
            Action::FullScreen => "full_screen",
            Action::QuickExport => "export",
            Action::Delete => "delete",
            Action::Back => "back",
            Action::Previous => "previous",
            Action::Next => "next",
            Action::First => "first",
            Action::Last => "last",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::OpenFolder => "Open a folder",
            Action::Settings => "Open or close Settings",
            Action::SelectAll => "Select all",
            Action::OpenPhoto => "Open the selected photo",
            Action::Pick => "Pick (again to unpick)",
            Action::PlayPause => "Play / pause a clip",
            Action::SkipBack => "Back 5 seconds in a clip",
            Action::SkipForward => "Forward 5 seconds in a clip",
            Action::FullScreen => "Full screen (in the viewer)",
            Action::QuickExport => "Quick Export",
            Action::Delete => "Delete (asks first)",
            Action::Back => "Clear selection / back to grid",
            Action::Previous => "Previous photo",
            Action::Next => "Next photo",
            Action::First => "First photo",
            Action::Last => "Last photo",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shortcut {
    key: Key,
    /// Ctrl on Windows, Cmd on macOS.
    command: bool,
    shift: bool,
    alt: bool,
}

impl Shortcut {
    const fn plain(key: Key) -> Self {
        Self { key, command: false, shift: false, alt: false }
    }

    const fn command(key: Key) -> Self {
        Self { key, command: true, shift: false, alt: false }
    }

    pub fn from_press(key: Key, modifiers: Modifiers) -> Self {
        Self { key, command: modifiers.command, shift: modifiers.shift, alt: modifiers.alt }
    }

    fn pressed(self, input: &InputState) -> bool {
        input.key_pressed(self.key)
            && input.modifiers.command == self.command
            && input.modifiers.shift == self.shift
            && input.modifiers.alt == self.alt
    }

    /// As shown to the user, like "Ctrl + O".
    fn text(self) -> String {
        let mut parts = Vec::new();
        if self.command {
            parts.push(platform::keys("Ctrl"));
        }
        if self.alt {
            parts.push(if cfg!(target_os = "macos") { "Option" } else { "Alt" }.to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        parts.push(key_text(self.key).to_string());
        parts.join(" + ")
    }

    /// As written in the settings file, like "command+O".
    fn saved(self) -> String {
        let mut text = String::new();
        for (on, name) in [(self.command, "command+"), (self.alt, "alt+"), (self.shift, "shift+")] {
            if on {
                text += name;
            }
        }
        text + self.key.name()
    }

    fn from_saved(text: &str) -> Option<Self> {
        let mut parts: Vec<&str> = text.split('+').collect();
        let key = Key::from_name(parts.pop()?)?;
        let mut shortcut = Self::plain(key);
        for part in parts {
            match part {
                "command" => shortcut.command = true,
                "alt" => shortcut.alt = true,
                "shift" => shortcut.shift = true,
                _ => return None,
            }
        }
        Some(shortcut)
    }
}

fn key_text(key: Key) -> &'static str {
    match key {
        Key::Escape => "Esc",
        // The key marked "delete" on a Mac keyboard is the one egui calls Backspace.
        Key::Backspace if cfg!(target_os = "macos") => "Delete",
        _ => key.symbol_or_name(),
    }
}

fn default_for(action: Action) -> Vec<Shortcut> {
    match action {
        Action::OpenFolder => vec![Shortcut::command(Key::O)],
        Action::Settings => vec![Shortcut::command(Key::Comma)],
        Action::SelectAll => vec![Shortcut::command(Key::A)],
        Action::OpenPhoto => vec![Shortcut::plain(Key::Enter)],
        Action::Pick => vec![Shortcut::plain(Key::P), Shortcut::plain(Key::Space)],
        // K plays and pauses in Premiere and DaVinci Resolve too.
        Action::PlayPause => vec![Shortcut::plain(Key::K)],
        Action::SkipBack => vec![Shortcut::plain(Key::J)],
        Action::SkipForward => vec![Shortcut::plain(Key::L)],
        Action::FullScreen => vec![Shortcut::plain(Key::F)],
        Action::QuickExport => vec![Shortcut::command(Key::E)],
        Action::Delete if cfg!(target_os = "macos") => vec![Shortcut::plain(Key::Backspace), Shortcut::plain(Key::Delete)],
        Action::Delete => vec![Shortcut::plain(Key::Delete)],
        Action::Back => vec![Shortcut::plain(Key::Escape)],
        Action::Previous => vec![Shortcut::plain(Key::ArrowLeft)],
        Action::Next => vec![Shortcut::plain(Key::ArrowRight)],
        Action::First => vec![Shortcut::plain(Key::Home)],
        Action::Last => vec![Shortcut::plain(Key::End)],
    }
}

/// Every action's shortcuts, in the order of `Action::ALL`.
#[derive(Clone, PartialEq, Debug)]
pub struct Keybinds(Vec<Vec<Shortcut>>);

impl Default for Keybinds {
    fn default() -> Self {
        Self(Action::ALL.into_iter().map(default_for).collect())
    }
}

impl Keybinds {
    fn get(&self, action: Action) -> &[Shortcut] {
        &self.0[action as usize]
    }

    pub fn pressed(&self, action: Action, input: &InputState) -> bool {
        self.get(action).iter().any(|s| s.pressed(input))
    }

    /// All of an action's shortcuts, like "P / Space".
    pub fn text(&self, action: Action) -> String {
        let mut names: Vec<String> = self.get(action).iter().map(|s| s.text()).collect();
        names.dedup();
        names.join("  /  ")
    }

    /// An action's main shortcut, like "Ctrl + O".
    pub fn main_text(&self, action: Action) -> String {
        self.get(action).first().map(|s| s.text()).unwrap_or_default()
    }

    /// An action's main shortcut for showing beside a button, like "Ctrl O".
    pub fn hint(&self, action: Action) -> String {
        self.main_text(action).replace(" + ", " ")
    }

    /// Makes `shortcut` the one key for `action`. An action that was already using it
    /// loses it; if that was its only key, it gets the key `action` just gave up, so no
    /// action is left with none.
    pub fn assign(&mut self, action: Action, shortcut: Shortcut) {
        let given_up = self.get(action).first().copied();
        for other in Action::ALL {
            let keys = &mut self.0[other as usize];
            if other != action && keys.contains(&shortcut) {
                keys.retain(|s| *s != shortcut);
                if keys.is_empty() {
                    keys.extend(given_up);
                }
            }
        }
        self.0[action as usize] = vec![shortcut];
    }

    /// Reads one line of the settings file, for the action called `name`. Anything it
    /// can't make sense of leaves the default.
    pub fn load(&mut self, name: &str, value: &str) {
        let Some(action) = Action::ALL.into_iter().find(|a| a.key() == name) else { return };
        let keys: Option<Vec<Shortcut>> = value.split('|').map(Shortcut::from_saved).collect();
        if let Some(keys) = keys.filter(|k| !k.is_empty()) {
            self.0[action as usize] = keys;
        }
    }

    /// (action name, shortcuts) for the settings file, only for what's been changed.
    pub fn changed(&self) -> Vec<(&'static str, String)> {
        Action::ALL
            .into_iter()
            .filter(|&a| self.get(a) != default_for(a))
            .map(|a| (a.key(), self.get(a).iter().map(|s| s.saved()).collect::<Vec<_>>().join("|")))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taking_a_used_key_swaps_rather_than_leaving_an_action_without_one() {
        let mut binds = Keybinds::default();
        let export = Shortcut::command(Key::E);
        binds.assign(Action::OpenFolder, export);
        assert_eq!(binds.get(Action::OpenFolder), [export]);
        assert_eq!(binds.get(Action::QuickExport), [Shortcut::command(Key::O)]);
    }

    #[test]
    fn taking_one_of_two_keys_leaves_the_other() {
        let mut binds = Keybinds::default();
        binds.assign(Action::Next, Shortcut::plain(Key::Space));
        assert_eq!(binds.get(Action::Pick), [Shortcut::plain(Key::P)]);
    }

    #[test]
    fn only_changes_are_saved_and_they_read_back() {
        let mut binds = Keybinds::default();
        assert!(binds.changed().is_empty());
        binds.assign(Action::Pick, Shortcut { key: Key::Comma, command: true, shift: true, alt: true });

        let mut back = Keybinds::default();
        let changed = binds.changed();
        for (name, value) in &changed {
            back.load(name, value);
        }
        assert_eq!(back, binds);
    }

    #[test]
    fn a_damaged_line_keeps_the_default() {
        let mut binds = Keybinds::default();
        binds.load("pick", "command+NotAKey");
        binds.load("nothing", "P");
        assert_eq!(binds, Keybinds::default());
    }
}
