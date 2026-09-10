//! The list of readable meetings, for the Markdown viewer.
//!
//! # Why this is not built on `queue::scan`
//!
//! The obvious implementation lists meetings from `meeting.json`. Measured on a
//! real library it would have shown **half of them**: 17 folders, 14 with a
//! `summary.md`, 7 with a `meeting.json`. The state file postdates most of the
//! recordings.
//!
//! So the question this asks of a folder is "is there something to read", not
//! "is there a state file". `meeting.json` is enrichment — a title, a duration —
//! and its absence is the normal case for anything recorded before it existed.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::queue;

/// The file this viewer exists to show.
pub const SUMMARY_FILENAME: &str = "summary.md";

/// One row in the list.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryEntry {
    /// The folder name. Stable, unique, and the id used to fetch the document —
    /// the frontend never sends a path.
    pub id: String,
    /// The user's title, or empty. Almost always empty in practice: on a real
    /// library not one meeting had been given a name.
    pub title: String,
    /// The first line of actual prose, for the row's second line. Empty if the
    /// summary is only headings and bullets.
    pub preview: String,
    /// Seconds of audio, when known.
    pub duration_seconds: Option<f64>,
}

/// Every meeting with a summary, **newest first**.
///
/// The opposite order from `queue::scan`, deliberately: a queue is worked
/// through oldest-first, and a library is read newest-first.
pub fn entries(output_folder: &Path) -> Vec<LibraryEntry> {
    let mut found: Vec<LibraryEntry> = queue::walk(output_folder)
        .into_iter()
        .filter(|folder| folder.path.join(SUMMARY_FILENAME).is_file())
        .map(|folder| {
            let id = folder
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            let summary = std::fs::read_to_string(folder.path.join(SUMMARY_FILENAME))
                .unwrap_or_default();

            LibraryEntry {
                id,
                title: folder
                    .state
                    .as_ref()
                    .map(|s| s.title.clone())
                    .unwrap_or_default(),
                preview: preview_of(&summary),
                duration_seconds: folder.state.as_ref().and_then(|s| s.duration_seconds),
            }
        })
        .collect();

    found.reverse();
    found
}

/// Resolve a library id to the summary file it names.
///
/// # Why an id and not a path
///
/// Every other file-touching command in this app derives its path from an id,
/// and this one keeps that property: the frontend cannot ask for a file by
/// naming it. `id` is matched against the folder's own name, so `..`, an
/// absolute path or a symlink target cannot be smuggled through — the result is
/// simply no match.
pub fn summary_path(output_folder: &Path, id: &str) -> Option<PathBuf> {
    // Rejected before touching the filesystem: an id is one path component, and
    // anything with a separator or a parent reference is not one.
    //
    // `:` is refused for two Windows-specific reasons that do not exist on
    // Unix. `PathBuf::push` documents that a path with a *prefix* replaces the
    // whole buffer, so `output_folder.join("C:x")` is `C:x` — relative to the
    // current directory of drive C, not to the output folder. And `a:b` is an
    // NTFS alternate data stream. Neither is reachable through a meeting title
    // (`text::sanitize_name` strips `:`), but this guard exists precisely
    // because the id arrives from the frontend rather than from a title.
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains(':')
        || id.contains('\0')
        || id == ".."
        || id == "."
    {
        return None;
    }

    let candidate = output_folder.join(id).join(SUMMARY_FILENAME);
    candidate.is_file().then_some(candidate)
}

/// The first line worth showing as a preview.
///
/// Skips headings, bullets and the bold-only lines the summary prompt produces
/// as section labels ("**Decisions**"), because a column of those says nothing
/// about which meeting a row is.
///
/// Not the first heading: measured across a real library, 11 of 14 first
/// headings read "Meeting Minutes" — the summary *type*, not the meeting.
fn preview_of(markdown: &str) -> String {
    markdown
        .lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with('*')
                && !line.starts_with('-')
                && !line.starts_with('>')
                && !line.starts_with('|')
                && !is_bold_label(line)
        })
        .unwrap_or_default()
        .to_string()
}

/// `**Decisions**` and nothing else on the line.
fn is_bold_label(line: &str) -> bool {
    line.starts_with("**") && line.ends_with("**") && line.len() > 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_skips_headings_labels_and_bullets() {
        let md = "## Meeting Minutes\n\n**Executive summary**\nThe team agreed to ship on Friday.\n\n* a bullet\n";
        assert_eq!(preview_of(md), "The team agreed to ship on Friday.");
    }

    #[test]
    fn a_summary_with_no_prose_previews_as_empty() {
        assert_eq!(preview_of("# Title\n\n* one\n* two\n"), "");
    }

    /// The guard, tested against a file that **exists and must not be reached**.
    ///
    /// The first version of this test pointed at `/tmp/does-not-matter` and
    /// asserted `None` for a list of hostile ids. It passed — and it passed just
    /// as well with the entire guard deleted, because a non-existent root makes
    /// the final `is_file()` reject everything. It proved nothing.
    ///
    /// So the target here is real: a `summary.md` in a sibling directory that a
    /// traversal would land on. Delete the guard and this fails.
    #[test]
    fn an_id_cannot_escape_the_output_folder() {
        let base = std::env::temp_dir().join(format!(
            "ma-escape-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let root = base.join("meetings");
        let secret = base.join("secret");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::create_dir_all(&secret).expect("mkdir");
        // The file a traversal is trying to reach. It is real, so `is_file()`
        // cannot be what refuses these ids.
        std::fs::write(secret.join(SUMMARY_FILENAME), "secret\n").expect("write");

        // Proof the target is reachable by path, so the guard is doing the work.
        assert!(
            root.join("..").join("secret").join(SUMMARY_FILENAME).is_file(),
            "the test fixture is wrong: nothing to escape to"
        );

        for id in [
            "../secret",
            "..\\secret",
            "../../etc",
            "..",
            ".",
            "a/b",
            "a\\b",
            "",
            "/etc/passwd",
            // Windows: a drive-relative path replaces the whole buffer.
            "C:secret",
            // Windows: an NTFS alternate data stream.
            "meetings:stream",
        ] {
            assert!(
                summary_path(&root, id).is_none(),
                "{id:?} resolved, so the guard did not stop it"
            );
        }

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_real_id_resolves_and_a_missing_one_does_not() {
        let root = std::env::temp_dir().join(format!("ma-library-{}", std::process::id()));
        let meeting = root.join("2026-09-10_10-00-00");
        std::fs::create_dir_all(&meeting).expect("mkdir");
        std::fs::write(meeting.join(SUMMARY_FILENAME), "# x\n\nbody\n").expect("write");

        assert!(summary_path(&root, "2026-09-10_10-00-00").is_some());
        assert!(summary_path(&root, "2026-09-10_10-00-01").is_none());

        // A folder without a summary is not readable, so it does not resolve.
        let empty = root.join("2026-09-10_11-00-00");
        std::fs::create_dir_all(&empty).expect("mkdir");
        assert!(summary_path(&root, "2026-09-10_11-00-00").is_none());

        let listed = entries(&root);
        assert_eq!(listed.len(), 1, "only the folder with a summary is listed");
        assert_eq!(listed[0].id, "2026-09-10_10-00-00");
        assert_eq!(listed[0].preview, "body");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_meeting_without_a_state_file_is_still_listed() {
        // The common case: 7 of 17 folders on a real machine had a meeting.json,
        // and a library built on that file would have hidden the rest.
        let root = std::env::temp_dir().join(format!("ma-library-nostate-{}", std::process::id()));
        let meeting = root.join("2026-09-03_18-02-10");
        std::fs::create_dir_all(&meeting).expect("mkdir");
        std::fs::write(meeting.join(SUMMARY_FILENAME), "Just prose.\n").expect("write");

        let listed = entries(&root);
        assert_eq!(listed.len(), 1, "no meeting.json must not mean no entry");
        assert_eq!(listed[0].title, "");
        assert_eq!(listed[0].duration_seconds, None);
        assert_eq!(listed[0].preview, "Just prose.");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_library_is_newest_first() {
        let root = std::env::temp_dir().join(format!("ma-library-order-{}", std::process::id()));
        for id in ["2026-09-01_09-00-00", "2026-09-05_09-00-00", "2026-09-03_09-00-00"] {
            let m = root.join(id);
            std::fs::create_dir_all(&m).expect("mkdir");
            std::fs::write(m.join(SUMMARY_FILENAME), "x\n").expect("write");
        }

        let ids: Vec<String> = entries(&root).into_iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            vec![
                "2026-09-05_09-00-00",
                "2026-09-03_09-00-00",
                "2026-09-01_09-00-00"
            ],
            "a library reads newest first, unlike the queue"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
