//! Unsaved changes to files open in editable viewers (2.2), such as a hex
//! editor's: the file's bytes with the changes made, and their undo
//! history.
//!
//! The first change reads the whole file into memory (at most
//! [`MAX_EDITABLE_BYTES`]), and the bytes are changed there, so the file on
//! disk can change or go without losing them. Saving writes them back;
//! reverting drops the buffer. Each step the extension makes (one
//! `edit-viewed-file` call) is undone at once.

use std::path::Path;

/// The largest file an editable viewer can change.
pub const MAX_EDITABLE_BYTES: u64 = 256 * 1024 * 1024;

/// Most changes in one step.
pub const MAX_CHANGES_PER_STEP: usize = 100_000;

/// Undo history kept, in bytes removed and inserted, past which the oldest
/// steps are forgotten.
const MAX_HISTORY_BYTES: usize = 64 * 1024 * 1024;

/// A change to make: `removed` bytes at `offset` replaced by `inserted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
  pub offset: u64,
  pub removed: u64,
  pub inserted: Vec<u8>,
}

/// A change made, with the bytes it removed, so it can be undone.
#[derive(Debug, Clone)]
struct Made {
  offset: usize,
  removed: Vec<u8>,
  inserted: Vec<u8>,
}

#[derive(Debug, Clone)]
struct Step {
  changes: Vec<Made>,
  /// The content before and after it: equal ids are equal bytes.
  before: u64,
  after: u64,
}

impl Step {
  fn weight(&self) -> usize {
    self
      .changes
      .iter()
      .map(|change| change.removed.len() + change.inserted.len() + 32)
      .sum()
  }
}

/// How the buffer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditState {
  pub size: u64,
  pub version: u64,
  pub dirty: bool,
  pub can_undo: bool,
  pub can_redo: bool,
}

impl EditState {
  /// The state of a file with no changes, at `version` (0 unless changes
  /// were reverted).
  pub fn unchanged(size: u64, version: u64) -> Self {
    EditState {
      size,
      version,
      dirty: false,
      can_undo: false,
      can_redo: false,
    }
  }
}

/// What an undo or redo changed: where, and how many bytes it left there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stepped {
  pub offset: u64,
  pub length: u64,
}

/// Why a step couldn't be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
  /// Made against another version: the current one.
  Stale(u64),
  /// A change outside the bytes.
  Range(String),
  /// Too many changes, or a file too large.
  TooLarge(String),
}

/// A file's bytes with unsaved changes.
#[derive(Debug)]
pub struct ByteBuffer {
  bytes: Vec<u8>,
  undo: Vec<Step>,
  redo: Vec<Step>,
  undo_weight: usize,
  /// Goes up with every change, undo, redo.
  version: u64,
  /// The content's id: what `Step::before`/`after` name.
  content: u64,
  /// The next new content id.
  next_content: u64,
  /// The content on disk: `None` once the steps to reach it are forgotten.
  saved: Option<u64>,
}

impl ByteBuffer {
  /// A buffer of a file's bytes, unchanged.
  pub fn new(bytes: Vec<u8>) -> Self {
    Self::at_version(bytes, 0)
  }

  /// A buffer of a file's bytes, unchanged, at `version`: after changes
  /// that were reverted, versions go on from theirs.
  pub fn at_version(bytes: Vec<u8>, version: u64) -> Self {
    ByteBuffer {
      bytes,
      undo: Vec::new(),
      redo: Vec::new(),
      undo_weight: 0,
      version,
      content: 0,
      next_content: 1,
      saved: Some(0),
    }
  }

  /// Reads a file into a buffer at `version`, refusing files too large to
  /// edit.
  pub fn read(path: &Path, version: u64) -> Result<Self, EditError> {
    let size = std::fs::metadata(path)
      .map_err(|error| EditError::Range(format!("{} couldn't be read: {error}", path.display())))?
      .len();
    if size > MAX_EDITABLE_BYTES {
      return Err(too_large_file());
    }
    let bytes = std::fs::read(path)
      .map_err(|error| EditError::Range(format!("{} couldn't be read: {error}", path.display())))?;
    if bytes.len() as u64 > MAX_EDITABLE_BYTES {
      return Err(too_large_file());
    }
    Ok(Self::at_version(bytes, version))
  }

  pub fn bytes(&self) -> &[u8] {
    &self.bytes
  }

  pub fn state(&self) -> EditState {
    EditState {
      size: self.bytes.len() as u64,
      version: self.version,
      dirty: self.saved != Some(self.content),
      can_undo: !self.undo.is_empty(),
      can_redo: !self.redo.is_empty(),
    }
  }

  /// Up to `length` bytes from `offset`.
  pub fn slice(&self, offset: u64, length: u64) -> &[u8] {
    let start = usize::try_from(offset)
      .unwrap_or(usize::MAX)
      .min(self.bytes.len());
    let end = start.saturating_add(usize::try_from(length).unwrap_or(usize::MAX));
    &self.bytes[start..end.min(self.bytes.len())]
  }

  /// Makes `changes` in order, as one step, if the buffer is at `version`.
  /// Nothing changes if one of them can't be made.
  pub fn edit(&mut self, version: u64, changes: Vec<Change>) -> Result<EditState, EditError> {
    if version != self.version {
      return Err(EditError::Stale(self.version));
    }
    if changes.len() > MAX_CHANGES_PER_STEP {
      return Err(EditError::TooLarge(format!(
        "at most {MAX_CHANGES_PER_STEP} changes at once"
      )));
    }
    // Checked first, against the sizes the changes before leave.
    let mut size = self.bytes.len() as u64;
    for change in &changes {
      let end = change.offset.checked_add(change.removed);
      if end.is_none_or(|end| end > size) {
        return Err(EditError::Range(format!(
          "{} bytes from offset {} are past the end ({size} bytes)",
          change.removed, change.offset
        )));
      }
      size = size - change.removed + change.inserted.len() as u64;
      if size > MAX_EDITABLE_BYTES {
        return Err(too_large_file());
      }
    }
    if changes
      .iter()
      .all(|change| change.removed == 0 && change.inserted.is_empty())
    {
      return Ok(self.state());
    }
    let made = changes
      .into_iter()
      .map(|change| {
        // In range: checked above.
        let offset = change.offset as usize;
        let removed = self
          .bytes
          .splice(
            offset..offset + change.removed as usize,
            change.inserted.iter().copied(),
          )
          .collect();
        Made {
          offset,
          removed,
          inserted: change.inserted,
        }
      })
      .collect();
    let before = self.content;
    self.content = self.next_content;
    self.next_content += 1;
    self.version += 1;
    // Redoing is no longer possible; the saved content may have been there.
    if self
      .redo
      .iter()
      .any(|step| Some(step.after) == self.saved || Some(step.before) == self.saved)
      && self.saved != Some(before)
    {
      self.saved = None;
    }
    self.redo.clear();
    let step = Step {
      changes: made,
      before,
      after: self.content,
    };
    self.undo_weight += step.weight();
    self.undo.push(step);
    self.forget_old_steps();
    Ok(self.state())
  }

  /// Forgets the oldest steps past [`MAX_HISTORY_BYTES`], keeping the last.
  fn forget_old_steps(&mut self) {
    while self.undo_weight > MAX_HISTORY_BYTES && self.undo.len() > 1 {
      let step = self.undo.remove(0);
      self.undo_weight -= step.weight();
      if self.saved == Some(step.before) {
        // The saved content can't be reached again.
        self.saved = None;
      }
    }
  }

  /// Undoes the last step.
  pub fn undo(&mut self) -> Option<Stepped> {
    let step = self.undo.pop()?;
    self.undo_weight -= step.weight();
    for change in step.changes.iter().rev() {
      self.bytes.splice(
        change.offset..change.offset + change.inserted.len(),
        change.removed.iter().copied(),
      );
    }
    self.content = step.before;
    self.version += 1;
    let stepped = extent(step.changes.iter().map(|c| (c.offset, c.removed.len())));
    self.redo.push(step);
    Some(stepped)
  }

  /// Makes the last step undone again.
  pub fn redo(&mut self) -> Option<Stepped> {
    let step = self.redo.pop()?;
    for change in &step.changes {
      self.bytes.splice(
        change.offset..change.offset + change.removed.len(),
        change.inserted.iter().copied(),
      );
    }
    self.content = step.after;
    self.version += 1;
    let stepped = extent(step.changes.iter().map(|c| (c.offset, c.inserted.len())));
    self.undo_weight += step.weight();
    self.undo.push(step);
    Some(stepped)
  }

  /// The bytes were written to the file.
  pub fn saved(&mut self) {
    self.saved = Some(self.content);
  }
}

fn too_large_file() -> EditError {
  EditError::TooLarge(format!(
    "files over {} MiB can't be edited",
    MAX_EDITABLE_BYTES / 1024 / 1024
  ))
}

/// The range from the first change's start to the last one's end, as the
/// bytes stand after the step.
fn extent(changes: impl Iterator<Item = (usize, usize)>) -> Stepped {
  let mut start = usize::MAX;
  let mut end = 0;
  for (offset, length) in changes {
    start = start.min(offset);
    end = end.max(offset + length);
  }
  if start == usize::MAX {
    return Stepped {
      offset: 0,
      length: 0,
    };
  }
  Stepped {
    offset: start as u64,
    length: end.saturating_sub(start) as u64,
  }
}

/// Writes `bytes` over a file, through a temporary file beside it that
/// replaces it, so a failure leaves the file as it was. The file keeps its
/// permissions.
pub fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  use std::io::Write;
  let folder = path.parent().unwrap_or(Path::new("."));
  let name = path
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_default();
  let permissions = std::fs::metadata(path).ok().map(|meta| meta.permissions());
  let temporary = folder.join(format!(".{name}.flareite-save"));
  let result = (|| {
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if let Some(permissions) = permissions {
      std::fs::set_permissions(&temporary, permissions)?;
    }
    std::fs::rename(&temporary, path)
  })();
  if result.is_err() {
    let _ = std::fs::remove_file(&temporary);
  }
  result
}

#[cfg(test)]
mod tests {
  use super::*;

  fn change(offset: u64, removed: u64, inserted: &[u8]) -> Change {
    Change {
      offset,
      removed,
      inserted: inserted.to_vec(),
    }
  }

  #[test]
  fn overwrites_inserts_and_removes_in_one_step() {
    let mut buffer = ByteBuffer::new(b"hello world".to_vec());
    let state = buffer
      .edit(
        0,
        vec![change(0, 1, b"H"), change(5, 0, b","), change(11, 1, b"")],
      )
      .expect("done");
    assert_eq!(buffer.bytes(), b"Hello, worl");
    assert_eq!(
      state,
      EditState {
        size: 11,
        version: 1,
        dirty: true,
        can_undo: true,
        can_redo: false
      }
    );
    assert_eq!(
      buffer.undo(),
      Some(Stepped {
        offset: 0,
        length: 12
      })
    );
    assert_eq!(buffer.bytes(), b"hello world");
    assert!(!buffer.state().dirty);
    assert_eq!(buffer.state().version, 2);
    buffer.redo().expect("done");
    assert_eq!(buffer.bytes(), b"Hello, worl");
    assert!(buffer.state().dirty);
  }

  #[test]
  fn refuses_stale_and_out_of_range_changes_whole() {
    let mut buffer = ByteBuffer::new(vec![1, 2, 3]);
    assert_eq!(
      buffer.edit(4, vec![change(0, 1, &[9])]),
      Err(EditError::Stale(0))
    );
    // The second goes past the end the first leaves.
    assert!(matches!(
      buffer.edit(0, vec![change(0, 1, &[]), change(1, 2, &[])]),
      Err(EditError::Range(_))
    ));
    assert!(matches!(
      buffer.edit(0, vec![change(u64::MAX, 2, &[])]),
      Err(EditError::Range(_))
    ));
    assert_eq!(buffer.bytes(), [1, 2, 3]);
    assert_eq!(buffer.state(), EditState::unchanged(3, 0));
    // Appending at the end is in range.
    buffer.edit(0, vec![change(3, 0, &[4])]).expect("done");
    assert_eq!(buffer.bytes(), [1, 2, 3, 4]);
  }

  #[test]
  fn tracks_the_saved_content_through_undo_and_redo() {
    let mut buffer = ByteBuffer::new(vec![0; 4]);
    buffer.edit(0, vec![change(0, 1, &[1])]).expect("done");
    buffer.saved();
    assert!(!buffer.state().dirty);
    buffer.undo();
    assert!(buffer.state().dirty);
    buffer.redo();
    assert!(!buffer.state().dirty);
    // Undone, then changed otherwise: the saved bytes can't come back.
    buffer.undo();
    buffer
      .edit(buffer.state().version, vec![change(1, 1, &[2])])
      .expect("done");
    assert!(buffer.state().dirty);
    buffer.undo();
    assert!(buffer.state().dirty);
    assert!(!buffer.state().can_redo || buffer.state().dirty);
  }

  #[test]
  fn reads_slices_past_the_end_short() {
    let buffer = ByteBuffer::new(vec![1, 2, 3]);
    assert_eq!(buffer.slice(1, 10), [2, 3]);
    assert_eq!(buffer.slice(5, 1), [] as [u8; 0]);
  }

  #[test]
  fn writes_files_in_place_of_the_old_ones() {
    let folder = tempfile::tempdir().expect("done");
    let path = folder.path().join("data.bin");
    std::fs::write(&path, [1, 2, 3]).expect("done");
    write_file(&path, &[4, 5]).expect("done");
    assert_eq!(std::fs::read(&path).expect("done"), [4, 5]);
    assert_eq!(std::fs::read_dir(folder.path()).expect("done").count(), 1);
  }
}
