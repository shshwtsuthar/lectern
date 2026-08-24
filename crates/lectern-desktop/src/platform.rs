//! Desktop-owned operating-system file actions.

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

use crossbeam_channel::{Receiver, Sender, bounded};
use eframe::egui;
use lectern_core::AssetId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlatformAction {
    Open,
    Reveal,
}

impl std::fmt::Display for PlatformAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => formatter.write_str("open"),
            Self::Reveal => formatter.write_str("reveal"),
        }
    }
}

pub(crate) trait AssetPlatform: Send + 'static {
    fn dispatch(&self, action: PlatformAction, path: &Path) -> Result<(), String>;
}

pub(crate) struct SystemAssetPlatform<L = ProcessLauncher> {
    launcher: L,
}

impl Default for SystemAssetPlatform {
    fn default() -> Self {
        Self {
            launcher: ProcessLauncher,
        }
    }
}

impl<L: Launcher + Send + 'static> AssetPlatform for SystemAssetPlatform<L> {
    fn dispatch(&self, action: PlatformAction, path: &Path) -> Result<(), String> {
        verify_readable_file(path)?;
        let command = platform_command(action, path)?;
        self.launcher.launch(&command.program, &command.arguments)
    }
}

pub(crate) struct NoopAssetPlatform;

impl AssetPlatform for NoopAssetPlatform {
    fn dispatch(&self, _action: PlatformAction, _path: &Path) -> Result<(), String> {
        Ok(())
    }
}

trait Launcher {
    fn launch(&self, program: &OsStr, arguments: &[OsString]) -> Result<(), String>;
}

pub(crate) struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn launch(&self, program: &OsStr, arguments: &[OsString]) -> Result<(), String> {
        Command::new(program)
            .args(arguments)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("could not start {}: {error}", Path::new(program).display()))
    }
}

struct PlatformCommand {
    program: OsString,
    arguments: Vec<OsString>,
}

#[cfg(target_os = "linux")]
fn platform_command(action: PlatformAction, path: &Path) -> Result<PlatformCommand, String> {
    let target = match action {
        PlatformAction::Open => path,
        PlatformAction::Reveal => path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| format!("{} has no parent folder to reveal", path.display()))?,
    };
    Ok(PlatformCommand {
        program: OsString::from("xdg-open"),
        arguments: vec![target.as_os_str().to_owned()],
    })
}

#[cfg(target_os = "macos")]
fn platform_command(action: PlatformAction, path: &Path) -> Result<PlatformCommand, String> {
    let mut arguments = Vec::with_capacity(2);
    if action == PlatformAction::Reveal {
        arguments.push(OsString::from("-R"));
    }
    arguments.push(path.as_os_str().to_owned());
    Ok(PlatformCommand {
        program: OsString::from("open"),
        arguments,
    })
}

#[cfg(target_os = "windows")]
fn platform_command(action: PlatformAction, path: &Path) -> Result<PlatformCommand, String> {
    match action {
        PlatformAction::Open => Ok(PlatformCommand {
            program: OsString::from("rundll32.exe"),
            arguments: vec![
                OsString::from("url.dll,FileProtocolHandler"),
                path.as_os_str().to_owned(),
            ],
        }),
        PlatformAction::Reveal => {
            let mut selection = OsString::from("/select,");
            selection.push(path.as_os_str());
            Ok(PlatformCommand {
                program: OsString::from("explorer.exe"),
                arguments: vec![selection],
            })
        }
    }
}

fn verify_readable_file(path: &Path) -> Result<(), String> {
    let metadata = path
        .metadata()
        .map_err(|error| format!("book file is unavailable: {error}"))?;
    if !metadata.is_file() {
        return Err(format!(
            "book file is unavailable: {} is not a regular file",
            path.display()
        ));
    }
    File::open(path)
        .map(|_| ())
        .map_err(|error| format!("book file is unreadable: {error}"))
}

struct PlatformRequest {
    asset_id: AssetId,
    action: PlatformAction,
    path: PathBuf,
}

pub(crate) struct PlatformEvent {
    pub(crate) asset_id: AssetId,
    pub(crate) action: PlatformAction,
    pub(crate) result: Result<(), String>,
}

pub(crate) struct PlatformWorker {
    sender: Sender<PlatformRequest>,
    events: Receiver<PlatformEvent>,
}

impl PlatformWorker {
    pub(crate) fn spawn(platform: impl AssetPlatform, context: &egui::Context) -> Self {
        let (sender, receiver) = bounded::<PlatformRequest>(1);
        let (event_sender, events) = bounded(1);
        let context = context.clone();
        let _worker = thread::Builder::new()
            .name("lectern-platform-actions".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    let result = platform.dispatch(request.action, &request.path);
                    if event_sender
                        .send(PlatformEvent {
                            asset_id: request.asset_id,
                            action: request.action,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    context.request_repaint();
                }
            });
        Self { sender, events }
    }

    pub(crate) fn dispatch(
        &self,
        asset_id: AssetId,
        action: PlatformAction,
        path: PathBuf,
    ) -> bool {
        self.sender
            .try_send(PlatformRequest {
                asset_id,
                action,
                path,
            })
            .is_ok()
    }

    pub(crate) fn next_event(&self) -> Option<PlatformEvent> {
        self.events.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::{OsStr, OsString},
        path::{Path, PathBuf},
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    use super::{Launcher, PlatformAction, SystemAssetPlatform};
    use crate::platform::AssetPlatform;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("lectern-platform-test-{}-{id}", std::process::id()));
            std::fs::create_dir(&path).expect("create temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove temporary directory");
        }
    }

    #[derive(Default)]
    struct FakeLauncher {
        calls: Mutex<Vec<(OsString, Vec<OsString>)>>,
    }

    impl Launcher for FakeLauncher {
        fn launch(&self, program: &OsStr, arguments: &[OsString]) -> Result<(), String> {
            self.calls
                .lock()
                .expect("lock calls")
                .push((program.to_owned(), arguments.to_vec()));
            Ok(())
        }
    }

    #[test]
    fn rejects_missing_paths_before_launch() {
        let launcher = FakeLauncher::default();
        let platform = SystemAssetPlatform { launcher };

        assert!(
            platform
                .dispatch(PlatformAction::Open, Path::new("missing book.epub"))
                .expect_err("missing file must fail")
                .contains("unavailable")
        );
        assert!(
            platform
                .launcher
                .calls
                .lock()
                .expect("lock calls")
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_arguments_preserve_spaces_quotes_and_non_unicode_paths() {
        let directory = TestDirectory::new();
        let name = OsString::from_vec(b"book with spaces 'quote' \xff.epub".to_vec());
        let path = directory.path().join(name);
        std::fs::write(&path, b"publication").expect("write publication");
        let platform = SystemAssetPlatform {
            launcher: FakeLauncher::default(),
        };

        platform
            .dispatch(PlatformAction::Open, &path)
            .expect("dispatch open");
        platform
            .dispatch(PlatformAction::Reveal, &path)
            .expect("dispatch reveal");

        let calls = platform.launcher.calls.lock().expect("lock calls");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1.last(), Some(&path.as_os_str().to_owned()));
        #[cfg(target_os = "linux")]
        assert_eq!(
            calls[1].1.last(),
            Some(&directory.path().as_os_str().to_owned())
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            calls[1].1,
            vec![OsString::from("-R"), path.into_os_string()]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_arguments_preserve_quotes_and_non_unicode_paths() {
        use std::os::windows::ffi::OsStringExt;

        use super::platform_command;

        let path = PathBuf::from(OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            b'\'' as u16,
            0xd800,
            b'.' as u16,
            b'p' as u16,
            b'd' as u16,
            b'f' as u16,
        ]));
        let open = platform_command(PlatformAction::Open, &path).expect("open command");
        assert_eq!(open.arguments.last(), Some(&path.as_os_str().to_owned()));

        let reveal = platform_command(PlatformAction::Reveal, &path).expect("reveal command");
        let mut expected = OsString::from("/select,");
        expected.push(path.as_os_str());
        assert_eq!(reveal.arguments, vec![expected]);
    }
}
