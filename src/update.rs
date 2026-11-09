//! Looks once, when the app opens, for a newer release on GitHub.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ureq::tls::{TlsConfig, TlsProvider};

const LATEST_RELEASE: &str = "https://api.github.com/repos/NuisanceCS/Cullery/releases/latest";
pub const DOWNLOAD_PAGE: &str = "https://github.com/NuisanceCS/Cullery/releases/latest";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Starts the check in the background. The result gets the newer version, if there is
/// one. Being offline, or GitHub not answering, just means nothing is shown.
pub fn check(ctx: eframe::egui::Context) -> Arc<Mutex<Option<String>>> {
    let found = Arc::new(Mutex::new(None));
    let shared = found.clone();
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        let Some(tag) = latest_tag(LATEST_RELEASE) else { return };
        if is_newer(&tag, CURRENT_VERSION) {
            *shared.lock().unwrap() = Some(tag.trim_start_matches('v').to_string());
            ctx.request_repaint();
        }
    });
    found
}

fn latest_tag(url: &str) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).build())
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .header("User-Agent", concat!("Cullery/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .ok()?;
    let body = response.body_mut().read_to_string().ok()?;
    tag_name(&body).map(str::to_owned)
}

/// The release's `tag_name` in GitHub's answer.
fn tag_name(json: &str) -> Option<&str> {
    const KEY: &str = "\"tag_name\"";
    let after = &json[json.find(KEY)? + KEY.len()..];
    after.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?.split('"').next()
}

/// Whether the release tagged `tag` (like "v1.2.0") is later than `current`. A tag that
/// isn't plain numbers counts as not newer.
fn is_newer(tag: &str, current: &str) -> bool {
    let numbers = |v: &str| v.trim_start_matches('v').split('.').map(|n| n.parse::<u32>().ok()).collect::<Option<Vec<u32>>>();
    matches!((numbers(tag), numbers(current)), (Some(tag), Some(current)) if tag > current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_as_numbers() {
        assert!(is_newer("v1.0.1", "1.0.0"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(is_newer("2.0.0", "1.9.9"));
        assert!(!is_newer("v1.0.0", "1.0.0"));
        assert!(!is_newer("v0.9.0", "1.0.0"));
        assert!(!is_newer("nightly", "1.0.0"));
        assert!(!is_newer("v2.0.0-beta", "1.0.0"));
    }

    #[test]
    fn finds_the_tag_in_githubs_answer() {
        assert_eq!(tag_name(r#"{"id":1,"tag_name": "v1.2.0","name":"x"}"#), Some("v1.2.0"));
        assert_eq!(tag_name(r#"{"message":"Not Found"}"#), None);
    }

    /// Reaches GitHub for real, through a public repository:
    /// cargo test --release real_release -- --ignored --nocapture
    #[test]
    #[ignore]
    fn reads_a_real_release() {
        let tag = latest_tag("https://api.github.com/repos/BurntSushi/ripgrep/releases/latest");
        println!("latest tag: {tag:?}");
        assert!(tag.is_some());
    }
}
