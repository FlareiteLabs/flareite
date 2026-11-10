//! Extensions' READMEs, shown on their pages in the workbench.
//!
//! The Markdown is turned into preview nodes by the editor's renderer (the
//! one extensions call with `workbench::render-markdown`), then checked and
//! resolved like any preview: links and pictures are found from the
//! extension's folder, and nothing becomes markup.

use std::path::Path;

use crate::preview::{self, PreviewContent};

/// Largest README shown.
pub const MAX_README_BYTES: u64 = 1024 * 1024;

/// The README at `path`, in the extension folder `dir`, as preview content.
pub fn render(path: &Path, dir: &Path) -> Result<PreviewContent, String> {
  let size = std::fs::metadata(path)
    .map_err(|error| error.to_string())?
    .len();
  if size > MAX_README_BYTES {
    return Err(format!(
      "the README is larger than {} MiB",
      MAX_README_BYTES / 1024 / 1024
    ));
  }
  let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
  let text = String::from_utf8_lossy(&bytes);
  preview::from_markdown(&text, Some(dir))
    .map_err(|error| format!("the README can't be shown: {error}"))
}

/// A picture the README shows, as a `data:` URL, if it's in the extension's
/// folder `dir`.
pub fn image(dir: &Path, path: &Path) -> Result<String, String> {
  let canonical = |path: &Path| std::fs::canonicalize(path).map_err(|error| error.to_string());
  if !canonical(path)?.starts_with(canonical(dir)?) {
    return Err("only pictures in the extension's folder are shown".into());
  }
  preview::load_image(path)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::preview::{ImageSource, LinkTarget, PreviewElement};

  #[test]
  fn renders_a_readme_with_links_and_pictures_from_its_folder() {
    let dir = tempfile::tempdir().expect("temp dir");
    let readme = dir.path().join("README.md");
    std::fs::write(
      &readme,
      "# Tidy\n\nSee [the guide](guide.md) and ![logo](logo.png).\n\n<script>x</script>\n",
    )
    .expect("write");
    std::fs::write(dir.path().join("logo.png"), b"\x89PNG\r\n\x1a\n").expect("write");
    let content = render(&readme, dir.path()).expect("renders");
    let elements: Vec<_> = content.nodes.iter().map(|node| &node.element).collect();
    assert!(matches!(elements[0], PreviewElement::Heading { level: 1 }));
    assert!(elements.iter().any(|element| matches!(
      element,
      PreviewElement::Link { target: LinkTarget::File { path, .. }, .. }
        if path.ends_with("guide.md")
    )));
    assert!(elements.iter().any(|element| matches!(
      element,
      PreviewElement::Image { source: ImageSource::File { path }, .. } if path.ends_with("logo.png")
    )));
    // HTML is reduced to its text.
    assert!(
      !elements
        .iter()
        .any(|element| matches!(element, PreviewElement::Text { text } if text.contains('<')))
    );

    assert!(image(dir.path(), &dir.path().join("logo.png")).is_ok());
    let outside = tempfile::tempdir().expect("temp dir");
    std::fs::write(outside.path().join("x.png"), b"\x89PNG\r\n\x1a\n").expect("write");
    assert!(image(dir.path(), &outside.path().join("x.png")).is_err());
  }
}
