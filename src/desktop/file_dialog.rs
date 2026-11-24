//! Native open dialogs for one file or one folder, through Rune's
//! `FileDialogs`: `NSOpenPanel` on macOS, the common item dialog on Windows,
//! and the FileChooser portal on Linux. Each runs on its own thread and
//! answers on a channel, so neither the window nor the runtime waits on the
//! person.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

fn show(dialog: Dialog, reply: Reply) {
    // A failed spawn drops the reply, which answers "nothing chosen".
    let _ = std::thread::Builder::new()
        .name("hex-file-dialog".into())
        .spawn(move || {
            let dialogs = rune::FileDialogs::native();
            let mut options = rune::FileDialogOptions {
                title: dialog.title,
                directory: dialog.directory.filter(|path| path.is_dir()),
                ..Default::default()
            };
            let choice = match dialog.target {
                Target::Folder => pollster::block_on(dialogs.open_folder(options)),
                Target::File(filters) => {
                    for filter in filters {
                        options = options.filter(filter.label, filter.extensions);
                    }
                    pollster::block_on(dialogs.open(options))
                }
            };
            reply.send(choice.map_err(|error| error.to_string()));
        });
}

pub(crate) struct Dialog {
    pub(crate) title: String,
    pub(crate) target: Target,
    /// Where the dialog starts, when it exists.
    pub(crate) directory: Option<PathBuf>,
}

pub(crate) enum Target {
    /// One existing file.
    File(Vec<Filter>),
    /// A folder: the macOS meetings and annotations folders, and Labs ›
    /// Screenshots' saves folder.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Folder,
}

/// A named set of extensions without the dot ("app", "exe"); `"*"` is any
/// file.
pub(crate) struct Filter {
    pub(crate) label: String,
    pub(crate) extensions: &'static [&'static str],
}

/// The chosen path, `None` when the person cancelled.
pub(crate) type Choice = Result<Option<PathBuf>, String>;

/// The two audio file pickers share one remembered folder, kept in Hex's
/// private application-support directory rather than in the app bundle.
/// Cancelled or failed selections must never replace it.
const LAST_AUDIO_DIRECTORY: &str = "last-audio-directory";

fn stored_audio_directory(file: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(file)
        .ok()
        .map(|saved| PathBuf::from(saved.trim_end_matches('\n')))
        .filter(|folder| folder.is_absolute() && folder.is_dir())
}

pub(crate) fn audio_directory() -> Option<PathBuf> {
    let previous = crate::app_paths::support_dir()
        .ok()
        .and_then(|root| stored_audio_directory(&root.join(LAST_AUDIO_DIRECTORY)));
    previous.or_else(crate::app_paths::audio_dir)
}

fn save_audio_directory(at: &Path, selected: &Path) -> std::io::Result<()> {
    let folder = selected
        .parent()
        .filter(|path| path.is_absolute() && path.is_dir());
    let Some(folder) = folder else {
        return Ok(());
    };
    if let Some(parent) = at.parent() {
        crate::app_paths::create_private_dir(parent)?;
    }
    std::fs::write(at, folder.to_string_lossy().as_bytes())
}

pub(crate) fn remember_audio_directory(selected: &Path) {
    let Ok(root) = crate::app_paths::support_dir() else {
        return;
    };
    if let Err(error) = save_audio_directory(&root.join(LAST_AUDIO_DIRECTORY), selected) {
        tracing::warn!(%error, "could not remember the audio file folder");
    }
}

/// Opens `dialog` without waiting for it; `wake` runs once it answers.
pub(crate) fn open(dialog: Dialog, wake: impl Fn() + Send + 'static) -> Pending {
    let (reply, pending) = channel(wake);
    show(dialog, reply);
    pending
}

fn channel(wake: impl Fn() + Send + 'static) -> (Reply, Pending) {
    let (sender, receiver) = sync_channel(1);
    let reply = Reply {
        sender,
        wake: Box::new(wake),
        answered: false,
    };
    (reply, Pending(receiver))
}

/// An open dialog's answer, polled from the window.
pub(crate) struct Pending(Receiver<Choice>);

impl Pending {
    /// `None` while the dialog is open.
    pub(crate) fn poll(&self) -> Option<Choice> {
        match self.0.try_recv() {
            Ok(choice) => Some(choice),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Ok(None)),
        }
    }
}

/// The dialog's one answer. Dropped unanswered (a dialog that never opened)
/// it answers "nothing chosen", so no caller waits on a dialog that is gone.
pub(crate) struct Reply {
    sender: SyncSender<Choice>,
    wake: Box<dyn Fn() + Send>,
    answered: bool,
}

impl Reply {
    pub(crate) fn send(mut self, choice: Choice) {
        self.answer(choice);
    }

    fn answer(&mut self, choice: Choice) {
        if std::mem::replace(&mut self.answered, true) {
            return;
        }
        let _ = self.sender.try_send(choice);
        (self.wake)();
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        self.answer(Ok(None));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn counted() -> (Arc<AtomicUsize>, impl Fn() + Send + 'static) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        (wakes, move || {
            counter.fetch_add(1, Ordering::Relaxed);
        })
    }

    #[test]
    fn the_chosen_path_arrives_once_and_wakes_the_window() {
        let (wakes, wake) = counted();
        let (reply, pending) = channel(wake);
        assert_eq!(pending.poll(), None);
        reply.send(Ok(Some(PathBuf::from("/Applications/Zed.app"))));
        assert_eq!(
            pending.poll(),
            Some(Ok(Some(PathBuf::from("/Applications/Zed.app"))))
        );
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_dialog_that_never_answers_reads_as_cancelled() {
        let (wakes, wake) = counted();
        let (reply, pending) = channel(wake);
        drop(reply);
        assert_eq!(pending.poll(), Some(Ok(None)));
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn audio_folder_survives_a_new_dialog_and_ignores_invalid_paths() {
        let root = std::env::temp_dir().join(format!(
            "hex-audio-folder-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let folder = root.join("selected recording folder");
        std::fs::create_dir_all(&folder).unwrap();
        let stored = root.join("last-audio-directory");
        save_audio_directory(&stored, &folder.join("interview.m4a")).unwrap();
        assert_eq!(stored_audio_directory(&stored), Some(folder.clone()));
        // A cancelled chooser never calls save_audio_directory; invalid files
        // also cannot overwrite the previous real folder.
        save_audio_directory(&stored, Path::new("/missing/recording.wav")).unwrap();
        assert_eq!(stored_audio_directory(&stored), Some(folder.clone()));
        std::fs::write(&stored, "/missing/audio-directory").unwrap();
        assert_eq!(stored_audio_directory(&stored), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failures_reach_the_caller() {
        let (_, wake) = counted();
        let (reply, pending) = channel(wake);
        reply.send(Err("no file chooser portal".into()));
        assert_eq!(pending.poll(), Some(Err("no file chooser portal".into())));
    }
}
