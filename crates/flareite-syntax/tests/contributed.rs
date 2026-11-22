//! Languages from WebAssembly grammars, using the HTML, CSS and SVG grammars
//! in `extensions/test-fixtures/languages/html-css`, and the JavaScript one
//! beside them for scripts in HTML and SVG.

use std::path::{Path, PathBuf};

use flareite_syntax::{
  Language, LanguageDefinition, Registry, SyntaxError, SyntaxTree, apply_edits, deadline,
  highlight_lines, parse,
};
use flareite_text::{ChangeSet, Edit, TextBuffer};

fn extension() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/test-fixtures/languages/html-css")
}

fn definition(id: &str, name: &str, injections: bool) -> LanguageDefinition {
  let dir = extension();
  LanguageDefinition {
    id: id.to_owned(),
    name: name.to_owned(),
    file_extensions: vec![id.to_owned()],
    file_names: Vec::new(),
    grammar: dir.join(format!("grammars/tree-sitter-{id}.wasm")),
    grammar_name: id.to_owned(),
    highlights: dir.join(format!("queries/{id}/highlights.scm")),
    injections: injections.then(|| dir.join(format!("queries/{id}/injections.scm"))),
    folds: None,
    symbols: None,
  }
}

fn javascript() -> LanguageDefinition {
  let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../../extensions/test-fixtures/languages/javascript");
  LanguageDefinition {
    id: "javascript".into(),
    name: "JavaScript".into(),
    file_extensions: vec!["js".into()],
    file_names: Vec::new(),
    grammar: dir.join("grammars/tree-sitter-javascript.wasm"),
    grammar_name: "javascript".into(),
    highlights: dir.join("queries/highlights.scm"),
    injections: None,
    folds: None,
    symbols: None,
  }
}

/// SVG, with the XML grammar.
fn svg() -> LanguageDefinition {
  let dir = extension();
  LanguageDefinition {
    id: "svg".into(),
    name: "SVG".into(),
    file_extensions: vec!["svg".into()],
    file_names: Vec::new(),
    grammar: dir.join("grammars/tree-sitter-xml.wasm"),
    grammar_name: "xml".into(),
    highlights: dir.join("queries/svg/highlights.scm"),
    injections: Some(dir.join("queries/svg/injections.scm")),
    folds: None,
    symbols: None,
  }
}

fn registry() -> Registry {
  let (registry, problems) = Registry::new([
    definition("html", "HTML", true),
    definition("css", "CSS", false),
    javascript(),
    svg(),
  ]);
  assert!(problems.is_empty(), "{problems:?}");
  registry
}

fn html(registry: &Registry) -> &Language {
  registry
    .for_path(Path::new("index.html"))
    .expect("html is registered")
}

fn parse_all(registry: &Registry, buffer: &TextBuffer, old: Option<&SyntaxTree>) -> SyntaxTree {
  parse(html(registry), registry, buffer, old, || false)
    .expect("grammar loads")
    .expect("parses")
}

/// Each line's spans as (text, kind).
fn spans(tree: &SyntaxTree, buffer: &TextBuffer) -> Vec<Vec<(String, &'static str)>> {
  highlight_lines(tree, buffer, 0..buffer.line_count(), 1 << 20)
    .into_iter()
    .enumerate()
    .map(|(line, spans)| {
      let text = buffer.line_text(line);
      spans
        .into_iter()
        .map(|span| (text[span.start..span.end].to_owned(), span.kind.name()))
        .collect()
    })
    .collect()
}

fn has(line: &[(String, &str)], text: &str, kind: &str) -> bool {
  line.iter().any(|(t, k)| t == text && *k == kind)
}

const PAGE: &str = "<!DOCTYPE html>
<!-- greeting -->
<p class=\"intro\">Hi &amp; bye</p>
<style>
  body > p { color: #333; margin: 0 4px; }
</style>
<script>
  const answer = 42;
</script>
";

#[test]
fn highlights_html_from_a_webassembly_grammar() {
  let registry = registry();
  let buffer = TextBuffer::from_text(PAGE);
  let tree = parse_all(&registry, &buffer, None);
  assert!(!tree.tree().root_node().has_error());
  let lines = spans(&tree, &buffer);
  assert!(
    has(&lines[0], "<!DOCTYPE html", "constant"),
    "{:?}",
    lines[0]
  );
  assert!(has(&lines[1], "<!-- greeting -->", "comment"));
  assert!(has(&lines[2], "p", "tag"));
  assert!(has(&lines[2], "class", "attribute"));
  assert!(has(&lines[2], "\"intro\"", "string"));
  assert!(has(&lines[2], "&amp;", "escape"));
  assert!(has(&lines[2], "<", "punctuation"));
}

#[test]
fn injected_css_and_javascript_are_highlighted() {
  let registry = registry();
  let buffer = TextBuffer::from_text(PAGE);
  let tree = parse_all(&registry, &buffer, None);
  let injected: Vec<&str> = tree
    .injections()
    .map(|(language, _)| language.id())
    .collect();
  assert_eq!(injected, ["css", "javascript"]);

  let lines = spans(&tree, &buffer);
  assert!(has(&lines[3], "style", "tag"), "{:?}", lines[3]);
  // CSS, from the extension's own CSS language.
  assert!(has(&lines[4], "body", "tag"), "{:?}", lines[4]);
  assert!(has(&lines[4], "color", "property"));
  assert!(has(&lines[4], "4", "number"));
  assert!(has(&lines[4], "px", "type"));
  // JavaScript, from another extension.
  assert!(has(&lines[7], "const", "keyword"), "{:?}", lines[7]);
  assert!(has(&lines[7], "42", "number") || has(&lines[7], "42", "constant"));
}

#[test]
fn incremental_parses_with_layers_match_full_parses() {
  let registry = registry();
  let mut buffer = TextBuffer::from_text(PAGE);
  let mut tree = parse_all(&registry, &buffer, None);
  let edits: [(usize, &str); 4] = [
    // Inside the CSS: a new declaration.
    (PAGE.find("margin").expect("css"), "padding: 1em; "),
    // Inside the JavaScript.
    (PAGE.find("42").expect("js"), "6 * 7 + "),
    // A new style element before the others.
    (
      PAGE.find("<style>").expect("style"),
      "<style>a { top: 1px }</style>\n",
    ),
    // Breaking the first style element apart.
    (0, "<script>let x = 1;</script>"),
  ];
  for (offset, text) in edits {
    let before = buffer.version();
    // Earlier edits only add text before later offsets, or at 0.
    let offset = offset.min(buffer.len());
    buffer
      .apply(&ChangeSet::single(Edit::insert(offset, text)).expect("valid"))
      .expect("applies");
    apply_edits(&mut tree, &buffer.edits_since(before).expect("edits"));
    tree = parse_all(&registry, &buffer, Some(&tree));
    let full = parse_all(&registry, &buffer, None);
    assert_eq!(
      tree
        .injections()
        .map(|(l, s)| (l.id().to_owned(), s))
        .collect::<Vec<_>>(),
      full
        .injections()
        .map(|(l, s)| (l.id().to_owned(), s))
        .collect::<Vec<_>>(),
    );
    assert_eq!(
      spans(&tree, &buffer),
      spans(&full, &buffer),
      "after inserting {text:?}"
    );
  }
  assert_eq!(tree.injections().count(), 4);
}

#[test]
fn css_files_parse_on_their_own() {
  let registry = registry();
  let css = registry.for_path(Path::new("site.CSS")).expect("css");
  let buffer =
    TextBuffer::from_text("/* x */\n:root { --gap: 2px; }\n@media screen { a { color: red } }\n");
  let tree = parse(css, &registry, &buffer, None, || false)
    .expect("grammar")
    .expect("parses");
  let lines = spans(&tree, &buffer);
  assert!(has(&lines[0], "/* x */", "comment"));
  assert!(has(&lines[2], "@media", "keyword"));
}

const ICON: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!-- an icon -->
<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\">
  <style>
    circle { fill: #333; }
  </style>
  <circle cx=\"12\" cy=\"12\" r=\"4\"/>
  <text>A &amp; B</text>
  <script><![CDATA[
    const size = 24;
  ]]></script>
</svg>
";

#[test]
fn svg_files_highlight_like_inline_svg_with_css_and_javascript_inside() {
  let registry = registry();
  let svg = registry.for_path(Path::new("icon.SVG")).expect("svg");
  let buffer = TextBuffer::from_text(ICON);
  let tree = parse(svg, &registry, &buffer, None, || false)
    .expect("grammar loads")
    .expect("parses");
  assert!(!tree.tree().root_node().has_error());
  let injected: Vec<&str> = tree
    .injections()
    .map(|(language, _)| language.id())
    .collect();
  assert_eq!(injected, ["css", "javascript"]);

  let lines = spans(&tree, &buffer);
  assert!(has(&lines[0], "xml", "keyword"), "{:?}", lines[0]);
  assert!(has(&lines[0], "version", "attribute"));
  assert!(has(&lines[1], "<!-- an icon -->", "comment"));
  assert!(has(&lines[2], "svg", "tag"), "{:?}", lines[2]);
  assert!(has(&lines[2], "viewBox", "attribute"));
  assert!(has(&lines[2], "\"0 0 24 24\"", "string"));
  assert!(has(&lines[2], "=", "operator"));
  assert!(has(&lines[2], "<", "punctuation"));
  // CSS in <style>.
  assert!(has(&lines[4], "circle", "tag"), "{:?}", lines[4]);
  assert!(has(&lines[4], "fill", "property"));
  assert!(has(&lines[6], "/>", "punctuation"), "{:?}", lines[6]);
  assert!(has(&lines[7], "&amp;", "escape"), "{:?}", lines[7]);
  // JavaScript in a CDATA section.
  assert!(has(&lines[9], "const", "keyword"), "{:?}", lines[9]);
}

#[test]
fn inline_svg_in_html_is_highlighted_as_html() {
  let registry = registry();
  let buffer = TextBuffer::from_text("<p><svg viewBox=\"0 0 8 8\"><circle r=\"4\"/></svg></p>\n");
  let tree = parse_all(&registry, &buffer, None);
  assert!(!tree.tree().root_node().has_error());
  let lines = spans(&tree, &buffer);
  assert!(has(&lines[0], "svg", "tag"), "{:?}", lines[0]);
  assert!(has(&lines[0], "circle", "tag"));
  assert!(has(&lines[0], "viewBox", "attribute"));
}

#[test]
fn stopped_parses_of_webassembly_grammars_give_up_and_recover() {
  let registry = registry();
  let big = TextBuffer::from_text(&"<div><p class=\"a\">text</p></div>\n".repeat(100_000));
  let stopped = parse(
    html(&registry),
    &registry,
    &big,
    None,
    deadline(std::time::Duration::ZERO),
  )
  .expect("grammar");
  assert!(stopped.is_none());
  let small = TextBuffer::from_text("<p>ok</p>");
  assert!(
    !parse_all(&registry, &small, None)
      .tree()
      .root_node()
      .has_error()
  );
}

#[test]
fn every_thread_can_parse_webassembly_grammars() {
  let registry = registry();
  std::thread::scope(|scope| {
    for _ in 0..4 {
      scope.spawn(|| {
        let buffer = TextBuffer::from_text(PAGE);
        assert_eq!(parse_all(&registry, &buffer, None).injections().count(), 2);
      });
    }
  });
}

#[test]
fn broken_grammars_and_queries_are_errors() {
  let dir = tempfile::tempdir().expect("temp dir");
  let garbage = dir.path().join("garbage.wasm");
  std::fs::write(&garbage, b"\0asm not really").expect("writes");
  let bad_query = dir.path().join("bad.scm");
  std::fs::write(&bad_query, "(no_such_node) @tag").expect("writes");

  let mut broken_grammar = definition("html", "HTML", false);
  broken_grammar.grammar = garbage;
  let mut broken_query = definition("css", "CSS", false);
  broken_query.highlights = bad_query;
  let (registry, _) = Registry::new([broken_grammar, broken_query]);
  let buffer = TextBuffer::from_text("<p></p>");
  for id in ["html", "css"] {
    let language = registry.get(id).expect("registered");
    let error = parse(language, &registry, &buffer, None, || false).expect_err("fails");
    match (id, &error) {
      ("html", SyntaxError::Load { .. }) | ("css", SyntaxError::Query { .. }) => {}
      _ => panic!("{id}: unexpected {error:?}"),
    }
  }
}

#[test]
fn injections_of_unknown_or_broken_languages_are_skipped() {
  // Without CSS registered, style elements stay plain.
  let (registry, _) = Registry::new([definition("html", "HTML", true), javascript()]);
  let buffer = TextBuffer::from_text(PAGE);
  let tree = parse_all(&registry, &buffer, None);
  let injected: Vec<&str> = tree
    .injections()
    .map(|(language, _)| language.id())
    .collect();
  assert_eq!(injected, ["javascript"]);
}
