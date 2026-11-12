//! Filesystem access for Flareite: loading and saving files, listing,
//! creating, renaming, moving, copying, and deleting entries for the
//! explorer, the Go to File index, find in files, change notifications, and
//! backups of unsaved documents. This crate holds no editor state.

mod backup;
mod create;
mod encoding;
mod entries;
mod file;
mod filters;
mod index;
mod paths;
mod replace;
mod search;
mod tree;
mod watcher;

pub use backup::{Abandoned, BackupMeta, BackupSession, RecoveredBackup};
pub use create::{Created, create_entry};
pub use encoding::{DecodeOptions, ENCODING_CHOICES, EncodeError, TextEncoding};
pub use entries::{EntryError, copy_entry, delete_entry, move_entry, rename_entry};
pub use file::{
  FileError, FileStamp, LoadedFile, load_text_file, load_text_file_with, save_text_file,
  write_file_atomically,
};
pub use filters::{ExcludeFilter, FilterError, split_patterns, to_slash};
pub use index::{FileIndex, FileMatch, MAX_INDEXED_FILES, files_in, scan_files};
pub use paths::normalize_path;
pub use replace::{MatchAt, Replacements, match_at, replacements};
pub use search::{
  FileMatches, LineMatch, SearchLimits, SearchScope, SearchSummary, search_workspace,
};
pub use tree::{DirEntry, EntryKind, list_directory};
pub use watcher::{WatchError, WorkspaceWatcher};
