//! The words that differ between Windows and macOS.

#[cfg(target_os = "macos")]
pub const TRASH: &str = "Trash";
#[cfg(not(target_os = "macos"))]
pub const TRASH: &str = "Recycle Bin";

/// A shortcut as written on this system: the key egui calls "command" is Ctrl on
/// Windows and Cmd on macOS.
pub fn keys(text: &str) -> String {
    #[cfg(target_os = "macos")]
    return text.replace("Ctrl", "Cmd");
    #[cfg(not(target_os = "macos"))]
    text.to_string()
}
