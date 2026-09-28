//! Markdown files opened from outside the app: "Open with", a double-click, a
//! drag onto the Dock icon, or the library's own Open button.
//!
//! # Why an id and not a path
//!
//! The library never lets the frontend name a file — see `library::summary_path`.
//! A file the OS hands us is the one exception to "every file lives under the
//! output folder", so it gets its own door with the same shape: Rust decides
//! which paths are readable, **registers** each one, and the frontend only ever
//! holds the id it was given back.
//!
//! The ids are `file:N`. The `:` is deliberate: `summary_path` refuses any id
//! containing one, so a document id can never be mistaken for a meeting, and a
//! meeting id can never be mistaken for a document.
//!
//! The registry lives for the session. Nothing is persisted: a file opened
//! yesterday is not something the app should keep reading today.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// The prefix that marks an id as an opened file rather than a meeting.
pub const ID_PREFIX: &str = "file:";

/// Extensions accepted as Markdown. Compared case-insensitively.
///
/// The same list is declared to the OS in `tauri.conf.json`. Anything else is
/// refused even if it arrives on the command line, so a stray argument cannot
/// turn this into a viewer for arbitrary files.
pub const EXTENSIONS: [&str; 4] = ["md", "markdown", "mdown", "mkd"];

/// One opened file, as the library list shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenedDocument {
    pub id: String,
    /// The file name, with its extension.
    pub name: String,
    /// The folder it lives in, for the header. Display only.
    pub folder: String,
}

/// The files opened this session, in the order they were opened.
#[derive(Default)]
pub struct Documents {
    paths: Mutex<Vec<PathBuf>>,
}

impl Documents {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept a file and return its id, or `None` if it is not a readable
    /// Markdown file.
    ///
    /// Opening the same file twice returns the same id, so a second double-click
    /// selects the document already listed instead of adding a duplicate row.
    pub fn register(&self, path: &Path) -> Option<String> {
        if !is_markdown(path) || !path.is_file() {
            return None;
        }
        // Canonical, so `./a.md` and `/full/path/a.md` are one document.
        let path = std::fs::canonicalize(path).ok()?;

        let mut paths = self.paths.lock().expect("documents poisoned");
        let index = match paths.iter().position(|p| *p == path) {
            Some(index) => index,
            None => {
                paths.push(path);
                paths.len() - 1
            }
        };
        Some(format!("{ID_PREFIX}{index}"))
    }

    /// The path behind an id handed out by [`Self::register`].
    pub fn path(&self, id: &str) -> Option<PathBuf> {
        let index: usize = id.strip_prefix(ID_PREFIX)?.parse().ok()?;
        self.paths
            .lock()
            .expect("documents poisoned")
            .get(index)
            .cloned()
    }

    /// Every opened file, **most recently opened first** — the same order the
    /// library reads meetings in.
    pub fn list(&self) -> Vec<OpenedDocument> {
        let paths = self.paths.lock().expect("documents poisoned");
        paths
            .iter()
            .enumerate()
            .rev()
            .map(|(index, path)| OpenedDocument {
                id: format!("{ID_PREFIX}{index}"),
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                folder: path
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
            .collect()
    }
}

/// Whether `id` names an opened file rather than a meeting.
pub fn is_document_id(id: &str) -> bool {
    id.starts_with(ID_PREFIX)
}

/// Whether the path has a Markdown extension.
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|ext| EXTENSIONS.contains(&ext.as_str()))
}

/// The Markdown files named on a command line.
///
/// This is how Windows and Linux hand a file to the app: the installer
/// registers `"app.exe" "%1"`, so a double-click is a launch with one argument.
/// A second launch while the app is running is caught by the single-instance
/// plugin, which forwards that instance's `argv` and working directory here.
///
/// `args` must **not** include the program name. Relative paths are resolved
/// against `cwd`, which for a forwarded launch is the *other* process's
/// directory, not this one's. Anything that is not an existing Markdown file —
/// a flag, a typo, a folder — is ignored.
pub fn markdown_args<I>(args: I, cwd: &Path) -> Vec<PathBuf>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    args.into_iter()
        .map(|arg| {
            let path = PathBuf::from(arg.into());
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .filter(|path| is_markdown(path) && path.is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ma-documents-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn only_markdown_files_are_accepted() {
        let dir = scratch("accept");
        std::fs::write(dir.join("notes.md"), "# hi\n").expect("write");
        std::fs::write(dir.join("README.MARKDOWN"), "# hi\n").expect("write");
        std::fs::write(dir.join("secrets.txt"), "no\n").expect("write");
        std::fs::create_dir_all(dir.join("folder.md")).expect("mkdir");

        let docs = Documents::new();
        assert!(docs.register(&dir.join("notes.md")).is_some());
        assert!(docs.register(&dir.join("README.MARKDOWN")).is_some(), "extension is case-insensitive");
        assert!(docs.register(&dir.join("secrets.txt")).is_none(), "not Markdown");
        assert!(docs.register(&dir.join("folder.md")).is_none(), "a folder is not a file");
        assert!(docs.register(&dir.join("missing.md")).is_none(), "does not exist");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_same_file_twice_is_one_document() {
        let dir = scratch("dedupe");
        std::fs::write(dir.join("a.md"), "a\n").expect("write");
        std::fs::write(dir.join("b.md"), "b\n").expect("write");

        let docs = Documents::new();
        let a = docs.register(&dir.join("a.md")).expect("a");
        let b = docs.register(&dir.join("b.md")).expect("b");
        let again = docs.register(&dir.join(".").join("a.md")).expect("a again");

        assert_ne!(a, b);
        assert_eq!(a, again, "a different spelling of the same path is the same id");
        assert_eq!(docs.list().len(), 2);
        assert_eq!(docs.list()[0].name, "b.md", "most recently opened first");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_id_resolves_only_to_what_was_registered() {
        let dir = scratch("resolve");
        std::fs::write(dir.join("a.md"), "a\n").expect("write");

        let docs = Documents::new();
        let id = docs.register(&dir.join("a.md")).expect("a");
        assert!(docs.path(&id).is_some_and(|p| p.ends_with("a.md")));

        for bogus in ["file:1", "file:-1", "file:", "file:../a.md", "a.md", "0", ""] {
            assert!(docs.path(bogus).is_none(), "{bogus:?} resolved");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_document_id_is_never_a_meeting_id() {
        // `summary_path` is what keeps these apart. If it ever stops refusing
        // `:`, a meeting folder called `file:0` could shadow an opened file.
        let dir = scratch("disjoint");
        let docs = Documents::new();
        std::fs::write(dir.join("a.md"), "a\n").expect("write");
        let id = docs.register(&dir.join("a.md")).expect("a");

        assert!(is_document_id(&id));
        assert!(crate::library::summary_path(&dir, &id).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn command_line_arguments_are_filtered_and_resolved() {
        let dir = scratch("args");
        std::fs::write(dir.join("notes.md"), "x\n").expect("write");
        std::fs::write(dir.join("image.png"), "x\n").expect("write");

        let found = markdown_args(
            ["--flag", "notes.md", "image.png", "missing.md"],
            &dir,
        );
        assert_eq!(found, vec![dir.join("notes.md")]);

        let absolute = dir.join("notes.md");
        let found = markdown_args([absolute.clone()], Path::new("/elsewhere"));
        assert_eq!(found, vec![absolute], "an absolute path ignores cwd");

        std::fs::remove_dir_all(&dir).ok();
    }
}
