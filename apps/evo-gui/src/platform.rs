//! Platform seam: every dialog, clipboard, browser, network, process and backend-opening call the
//! app makes goes through [`Platform`], so tests swap in [`FakePlatform`] and never open a real
//! dialog, browser or connection.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::backend::fake::FakeBackend;
use crate::backend::machine::MachineBackend;
use crate::backend::{EmuBackend, Frame};

/// Largest HTTP reply body [`DesktopPlatform::http_get`] reads (the update check needs a few KiB).
pub const HTTP_BODY_LIMIT: u64 = 1024 * 1024;

fn update_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// One file-type filter of an open or save dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileFilter {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
}

/// The host services the app uses.
pub trait Platform: Send + Sync + fmt::Debug {
    /// Asks for a file to open; `None` when the user cancels.
    fn pick_open(&self, title: &str, filters: &[FileFilter]) -> Option<PathBuf>;
    fn pick_open_many(&self, title: &str, filters: &[FileFilter]) -> Vec<PathBuf>;
    /// Chooses a folder for a batch of received calculator files.
    fn pick_folder(&self, title: &str) -> Option<PathBuf>;
    /// Asks for a file to save to; `None` when the user cancels.
    fn pick_save(&self, title: &str, default_name: &str, filters: &[FileFilter])
    -> Option<PathBuf>;
    /// Opens the OS bundle `bundle`, keeping the calculator's flash in `nv_dir`.
    fn open_backend(&self, bundle: &Path, nv_dir: &Path) -> Result<Box<dyn EmuBackend>, String>;
    /// Puts `frame` on the clipboard as an image.
    fn copy_image(&self, frame: &Frame) -> Result<(), String>;
    /// Opens `url` in the default browser.
    fn open_url(&self, url: &str) -> Result<(), String>;
    /// Fetches `url` as text. Blocking: call it from a worker thread. A non-2xx reply is
    /// `Err("HTTP {code}")`.
    fn http_get(&self, url: &str) -> Result<String, String>;
    /// Bounded binary download; an oversized body is an error, never a truncated success.
    fn http_get_bytes(&self, url: &str, limit: usize) -> Result<Vec<u8>, String>;
    /// Starts the current executable again with `args`.
    fn spawn_instance(&self, args: &[String]) -> Result<(), String>;
}

/// The platform shared by the app and its worker threads.
pub type PlatformRef = Arc<dyn Platform>;

/// The real desktop: native dialogs, the system clipboard and browser, HTTPS and processes.
#[derive(Debug, Default)]
pub struct DesktopPlatform;

impl DesktopPlatform {
    pub fn shared() -> PlatformRef {
        Arc::new(Self)
    }
}

fn dialog(title: &str, filters: &[FileFilter]) -> rfd::FileDialog {
    filters
        .iter()
        .fold(rfd::FileDialog::new().set_title(title), |d, f| {
            d.add_filter(f.name, f.extensions)
        })
}

impl Platform for DesktopPlatform {
    fn pick_open_many(&self, title: &str, filters: &[FileFilter]) -> Vec<PathBuf> {
        dialog(title, filters).pick_files().unwrap_or_default()
    }

    fn http_get_bytes(&self, url: &str, limit: usize) -> Result<Vec<u8>, String> {
        let mut response = update_agent().get(url).call().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > limit {
            return Err(format!("Download exceeds the {} byte limit", limit));
        }
        Ok(bytes)
    }
    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).pick_folder()
    }
    fn pick_open(&self, title: &str, filters: &[FileFilter]) -> Option<PathBuf> {
        dialog(title, filters).pick_file()
    }

    fn pick_save(
        &self,
        title: &str,
        default_name: &str,
        filters: &[FileFilter],
    ) -> Option<PathBuf> {
        dialog(title, filters)
            .set_file_name(default_name)
            .save_file()
    }

    fn open_backend(&self, bundle: &Path, nv_dir: &Path) -> Result<Box<dyn EmuBackend>, String> {
        MachineBackend::open(bundle, nv_dir)
            .map(|b| Box::new(b) as Box<dyn EmuBackend>)
            .map_err(|e| e.to_string())
    }

    fn copy_image(&self, frame: &Frame) -> Result<(), String> {
        if !frame.is_valid() {
            return Err("no screen to copy".to_owned());
        }
        let image = arboard::ImageData {
            width: frame.width as usize,
            height: frame.height as usize,
            bytes: Cow::Borrowed(&frame.rgba),
        };
        arboard::Clipboard::new()
            .and_then(|mut c| c.set_image(image))
            .map_err(|e| e.to_string())
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        webbrowser::open(url).map_err(|e| e.to_string())
    }

    fn http_get(&self, url: &str) -> Result<String, String> {
        let reply = update_agent()
            .get(url)
            .header("User-Agent", concat!("evo-gui/", env!("CARGO_PKG_VERSION")))
            .header("Accept", "application/vnd.github+json")
            .call();
        let mut reply = match reply {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(c)) => return Err(format!("HTTP {c}")),
            Err(e) => return Err(e.to_string()),
        };
        let mut body = String::new();
        reply
            .body_mut()
            .as_reader()
            .take(HTTP_BODY_LIMIT)
            .read_to_string(&mut body)
            .map_err(|e| e.to_string())?;
        Ok(body)
    }

    fn spawn_instance(&self, args: &[String]) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::process::Command::new(exe)
            .args(args)
            .spawn()
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

/// Opens a backend from `(bundle, nv_dir)`, as [`Platform::open_backend`].
pub type BackendFactory =
    Box<dyn Fn(&Path, &Path) -> Result<Box<dyn EmuBackend>, String> + Send + Sync>;

struct FakeFactory(BackendFactory);

impl fmt::Debug for FakeFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FakeFactory")
    }
}

#[derive(Debug, Default)]
struct FakeState {
    binary_http: std::collections::BTreeMap<String, Result<Vec<u8>, String>>,
    open_many_answers: VecDeque<Vec<PathBuf>>,
    open_answers: VecDeque<Option<PathBuf>>,
    save_answers: VecDeque<Option<PathBuf>>,
    http: Option<Result<String, String>>,
    copy_error: Option<String>,
    factory: Option<FakeFactory>,
    asked: Vec<String>,
    copied: usize,
    urls: Vec<String>,
    spawned: Vec<Vec<String>>,
    opened: Vec<PathBuf>,
}

/// A scripted platform for tests: dialogs answer from queues, nothing touches the host, and every
/// call is logged.
#[derive(Debug, Default)]
pub struct FakePlatform {
    state: Mutex<FakeState>,
}

impl FakePlatform {
    pub fn queue_open_many(&self, paths: Vec<PathBuf>) {
        self.state().open_many_answers.push_back(paths);
    }
    pub fn set_http_bytes(&self, url: &str, reply: Result<Vec<u8>, String>) {
        self.state().binary_http.insert(url.into(), reply);
    }
    /// A new fake; `.clone()` of the result coerces to [`PlatformRef`].
    pub fn shared() -> Arc<FakePlatform> {
        Arc::new(Self::default())
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        // A test that panicked while holding the lock must not hide the state from the next call.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The answer of the next open dialog (FIFO; an empty queue answers `None`).
    pub fn queue_open(&self, answer: Option<PathBuf>) {
        self.state().open_answers.push_back(answer);
    }

    /// The answer of the next save dialog (FIFO; an empty queue answers `None`).
    pub fn queue_save(&self, answer: Option<PathBuf>) {
        self.state().save_answers.push_back(answer);
    }

    /// The reply of every later `http_get` (default `Err("offline")`).
    pub fn set_http(&self, reply: Result<String, String>) {
        self.state().http = Some(reply);
    }

    /// Makes every later `copy_image` fail with `msg`.
    pub fn fail_copy(&self, msg: &str) {
        self.state().copy_error = Some(msg.to_owned());
    }

    /// Replaces the default backend factory (a `FakeBackend` named after the bundle file).
    pub fn set_backend_factory(&self, f: BackendFactory) {
        self.state().factory = Some(FakeFactory(f));
    }

    /// Dialog titles, in the order asked.
    pub fn asked(&self) -> Vec<String> {
        self.state().asked.clone()
    }

    /// How many images were copied successfully.
    pub fn copied(&self) -> usize {
        self.state().copied
    }

    /// URLs opened in the browser, in order.
    pub fn urls(&self) -> Vec<String> {
        self.state().urls.clone()
    }

    /// Argument lists of the spawned instances, in order.
    pub fn spawned(&self) -> Vec<Vec<String>> {
        self.state().spawned.clone()
    }

    /// Bundles passed to `open_backend`, in order.
    pub fn opened(&self) -> Vec<PathBuf> {
        self.state().opened.clone()
    }
}

impl Platform for FakePlatform {
    fn pick_open_many(&self, title: &str, _filters: &[FileFilter]) -> Vec<PathBuf> {
        let mut s = self.state();
        s.asked.push(title.into());
        s.open_many_answers.pop_front().unwrap_or_default()
    }
    fn http_get_bytes(&self, url: &str, limit: usize) -> Result<Vec<u8>, String> {
        let bytes = self
            .state()
            .binary_http
            .get(url)
            .cloned()
            .unwrap_or_else(|| Err("offline".into()))?;
        if bytes.len() > limit {
            return Err("Download exceeds byte limit".into());
        }
        Ok(bytes)
    }
    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        let mut s = self.state();
        s.asked.push(title.to_owned());
        s.open_answers.pop_front().flatten()
    }
    fn pick_open(&self, title: &str, _filters: &[FileFilter]) -> Option<PathBuf> {
        let mut s = self.state();
        s.asked.push(title.to_owned());
        s.open_answers.pop_front().flatten()
    }

    fn pick_save(
        &self,
        title: &str,
        _default_name: &str,
        _filters: &[FileFilter],
    ) -> Option<PathBuf> {
        let mut s = self.state();
        s.asked.push(title.to_owned());
        s.save_answers.pop_front().flatten()
    }

    fn open_backend(&self, bundle: &Path, nv_dir: &Path) -> Result<Box<dyn EmuBackend>, String> {
        // Take the factory out so it runs without the lock held (it may call back into the fake).
        let factory = {
            let mut s = self.state();
            s.opened.push(bundle.to_path_buf());
            s.factory.take()
        };
        let Some(factory) = factory else {
            let name = bundle
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| bundle.display().to_string());
            return Ok(Box::new(FakeBackend::new().named(&name)));
        };
        let result = (factory.0)(bundle, nv_dir);
        let mut s = self.state();
        if s.factory.is_none() {
            s.factory = Some(factory);
        }
        result
    }

    fn copy_image(&self, _frame: &Frame) -> Result<(), String> {
        let mut s = self.state();
        match &s.copy_error {
            Some(msg) => Err(msg.clone()),
            None => {
                s.copied += 1;
                Ok(())
            }
        }
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        self.state().urls.push(url.to_owned());
        Ok(())
    }

    fn http_get(&self, _url: &str) -> Result<String, String> {
        self.state()
            .http
            .clone()
            .unwrap_or_else(|| Err("offline".to_owned()))
    }

    fn spawn_instance(&self, args: &[String]) -> Result<(), String> {
        self.state().spawned.push(args.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn update_http_has_a_finite_whole_request_timeout() {
        // Uses the production agent configuration, independent of FakePlatform.
        assert_eq!(
            super::update_agent().config().timeouts().global,
            Some(std::time::Duration::from_secs(30))
        );
    }
}
