//! The update check: asks the GitHub releases API for the latest release on a worker thread
//! and compares its tag with this build's version.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use serde::Deserialize;

use crate::platform::PlatformRef;

/// The GitHub repository releases are published from.
pub const UPDATE_REPO: &str = "connerglover/ti84evo-emu";

/// Where "Report a bug" sends the browser.
pub const ISSUES_URL: &str = "https://github.com/connerglover/ti84evo-emu/issues/new";

/// The GitHub API endpoint of the latest release of [`UPDATE_REPO`].
pub fn latest_release_url() -> String {
    format!("https://api.github.com/repos/{UPDATE_REPO}/releases/latest")
}

/// Parses `"a.b.c"` with an optional leading `v`; anything after the first `-` is ignored.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let core = s.split('-').next().unwrap_or(s);
    let mut parts = core.split('.').map(|p| {
        if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) {
            p.parse::<u64>().ok()
        } else {
            None
        }
    });
    let v = (parts.next()??, parts.next()??, parts.next()??);
    match parts.next() {
        None => Some(v),
        Some(_) => None,
    }
}

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    /// The release's web page (always `https://`).
    pub url: String,
    pub version: (u64, u64, u64),
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    html_url: String,
}

/// Reads the `tag_name` and `html_url` of a GitHub release reply.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let r: ReleaseJson =
        serde_json::from_str(json).map_err(|e| format!("unexpected reply ({e})"))?;
    let version = parse_version(&r.tag_name)
        .ok_or_else(|| format!("unrecognized version tag {:?}", r.tag_name))?;
    // The page is opened in the browser: never hand it another scheme from the network.
    if !r.html_url.starts_with("https://") {
        return Err("the release page is not an https:// URL".to_owned());
    }
    Ok(Release {
        tag: r.tag_name,
        url: r.html_url,
        version,
    })
}

/// What one update check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// A newer release exists.
    Newer(Release),
    /// The latest release is this version or older.
    Latest,
    /// The repository has no published release (`HTTP 404`).
    NoReleases,
    /// The request or the reply failed.
    Failed(String),
}

/// Classifies `reply` (from [`crate::platform::Platform::http_get`]) against `current`.
pub fn outcome(reply: Result<String, String>, current: &str) -> UpdateOutcome {
    let body = match reply {
        Ok(b) => b,
        Err(e) if e == "HTTP 404" => return UpdateOutcome::NoReleases,
        Err(e) => return UpdateOutcome::Failed(e),
    };
    match parse_release(&body) {
        Ok(r) if r.version > parse_version(current).unwrap_or((0, 0, 0)) => UpdateOutcome::Newer(r),
        Ok(_) => UpdateOutcome::Latest,
        Err(e) => UpdateOutcome::Failed(e),
    }
}

/// A running update check.
pub struct UpdateCheck {
    rx: Receiver<Result<String, String>>,
}

impl UpdateCheck {
    /// Starts fetching [`latest_release_url`] on a worker thread.
    pub fn spawn(platform: PlatformRef) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("update-check".to_owned())
            .spawn(move || {
                // The app may have closed meanwhile; then nobody listens.
                let _ = tx.send(platform.http_get(&latest_release_url()));
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { rx })
    }

    /// The reply once the worker finished; `None` while it runs.
    pub fn poll(&self) -> Option<Result<String, String>> {
        match self.rx.try_recv() {
            Ok(reply) => Some(reply),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("the update check stopped".to_owned())),
        }
    }
}
