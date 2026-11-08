//! What Cullery was asked to open when it started:
//!
//! ```text
//! cullery.exe [FOLDER_OR_PHOTO] [--theme NAME]
//! ```

use std::ffi::OsString;
use std::path::PathBuf;

use crate::theme::ThemeId;

#[derive(Debug, Default, PartialEq)]
pub struct StartArgs {
    /// A folder to open, or a photo to open its folder at.
    pub path: Option<PathBuf>,
    /// A theme for this run only; the saved one is left alone.
    pub theme: Option<ThemeId>,
    /// Something on the command line that couldn't be used, to show in the window
    /// (a windowed app has no console to print it to).
    pub problem: Option<String>,
}

impl StartArgs {
    /// Reads the arguments that follow the program's name.
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Self {
        let mut out = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let text = arg.to_string_lossy();
            if let Some(rest) = text.strip_prefix("--") {
                let (name, inline) = match rest.split_once('=') {
                    Some((name, value)) => (name.to_string(), Some(value.to_string())),
                    None => (rest.to_string(), None),
                };
                if name != "theme" {
                    out.problem = Some(format!("Unknown option: --{name}"));
                    continue;
                }
                let value = inline.or_else(|| args.next().map(|v| v.to_string_lossy().into_owned()));
                match value.as_deref().and_then(ThemeId::from_key) {
                    Some(theme) => out.theme = Some(theme),
                    None => out.problem = Some(format!("Unknown theme: {}", value.unwrap_or_default())),
                }
            } else if out.path.is_none() {
                out.path = Some(clean_path(arg));
            }
        }
        out
    }
}

/// Windows passes `"F:\"` to a program as `F:"` plus a stray quote; drop the quote and
/// put back the backslash a drive letter needs.
fn clean_path(arg: OsString) -> PathBuf {
    let Some(text) = arg.to_str() else { return PathBuf::from(arg) };
    let Some(trimmed) = text.strip_suffix('"') else { return PathBuf::from(arg) };
    let is_drive = trimmed.len() == 2 && trimmed.ends_with(':');
    PathBuf::from(if is_drive { format!("{trimmed}\\") } else { trimmed.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> StartArgs {
        StartArgs::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn reads_a_path_and_a_theme_in_any_order() {
        let expected = StartArgs {
            path: Some(PathBuf::from(r"F:\DCIM\100CANON")),
            theme: Some(ThemeId::Win10Dark),
            problem: None,
        };
        assert_eq!(parse(&[r"F:\DCIM\100CANON", "--theme", "win10-dark"]), expected);
        assert_eq!(parse(&["--theme=win10-dark", r"F:\DCIM\100CANON"]), expected);
        assert_eq!(parse(&[]), StartArgs::default());
    }

    #[test]
    fn reports_what_it_cannot_use() {
        assert_eq!(parse(&["--theme", "purple"]).problem.as_deref(), Some("Unknown theme: purple"));
        assert_eq!(parse(&["--theme"]).problem.as_deref(), Some("Unknown theme: "));
        assert_eq!(parse(&["--nope"]).problem.as_deref(), Some("Unknown option: --nope"));
    }

    #[test]
    fn repairs_a_quoted_trailing_backslash() {
        assert_eq!(parse(&["F:\""]).path, Some(PathBuf::from("F:\\")));
        assert_eq!(parse(&[r#"D:\Photos""#]).path, Some(PathBuf::from(r"D:\Photos")));
    }
}
