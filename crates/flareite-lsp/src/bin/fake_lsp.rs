//! A scripted language server for `flareite-lsp`'s tests.
//!
//! It keeps each open document's text by applying the changes it receives
//! (counting columns its own way, independently of the client), reports a
//! diagnostic for every occurrence of the word `error`, and answers hover,
//! definition, references, symbol and completion requests from the text
//! (its symbols are the functions declared with `fn`). `fake/text` returns a
//! document's text so tests can check synchronization; `fake/sleep` delays
//! its answer. Flags: `--utf16` negotiates UTF-16 positions instead of
//! UTF-8; `--crash-on-hover` exits when asked for a hover; `--ignore-exit`
//! ignores `exit` and only stops when its input closes; `--stubborn` starts
//! a long-lived process that shares its output, and waits for it instead of
//! stopping.
//!
//! Once initialized it watches `**/*.watched` files, and reports the
//! changes it hears of as `fake/watched`. When its configuration changes it
//! asks for its `fake` section and reports the answer as `fake/configured`.
//!
//! Editing features: formatting removes spaces at the ends of lines; rename
//! replaces every occurrence of the word in its document; each `error` has a
//! quick fix, "Replace with 'fixed'", whose edit comes from
//! `codeAction/resolve`, and every request for code actions also offers
//! "Apply by Command", running `fake.apply`, which asks the client to apply
//! an edit (`workspace/applyEdit`) and answers with what the client said.
//! Signature help shows `f(a, b)` inside parentheses.

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::time::Duration;

use flareite_lsp::framing::{read_message, write_message};
use serde_json::{Value, json};

struct Server {
  utf16: bool,
  crash_on_hover: bool,
  ignore_exit: bool,
  documents: HashMap<String, String>,
  /// Each document's version, as the client last said.
  versions: HashMap<String, Value>,
  /// The `workspace/executeCommand` waiting for the client's answer to
  /// `workspace/applyEdit`.
  executing: Option<Value>,
  out: std::io::Stdout,
}

impl Server {
  fn send(&mut self, message: Value) {
    let body = serde_json::to_vec(&message).expect("serializes");
    write_message(&mut self.out, &body).expect("writes");
    self.out.flush().expect("flushes");
  }

  /// The byte offset of an LSP position, lines split on `\n`.
  fn offset(&self, text: &str, position: &Value) -> usize {
    let line = position["line"].as_u64().unwrap_or(0) as usize;
    let character = position["character"].as_u64().unwrap_or(0) as usize;
    let mut start = 0;
    for _ in 0..line {
      match text[start..].find('\n') {
        Some(index) => start += index + 1,
        None => return text.len(),
      }
    }
    let line_text = &text[start
      ..text[start..]
        .find('\n')
        .map_or(text.len(), |end| start + end)];
    let mut units = 0;
    for (index, ch) in line_text.char_indices() {
      if units >= character {
        return start + index;
      }
      units += if self.utf16 {
        ch.len_utf16()
      } else {
        ch.len_utf8()
      };
    }
    start + line_text.len()
  }

  fn position(&self, text: &str, offset: usize) -> Value {
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let column: usize = if self.utf16 {
      before[line_start..].chars().map(char::len_utf16).sum()
    } else {
      offset - line_start
    };
    json!({ "line": line, "character": column })
  }

  fn publish(&mut self, uri: &str, version: Option<&Value>) {
    let text = self.documents.get(uri).cloned().unwrap_or_default();
    let diagnostics: Vec<Value> = text
      .match_indices("error")
      .map(|(offset, word)| {
        json!({
          "range": { "start": self.position(&text, offset), "end": self.position(&text, offset + word.len()) },
          "severity": 1,
          "source": "fake",
          "message": "found error",
        })
      })
      .collect();
    self.send(json!({
      "jsonrpc": "2.0",
      "method": "textDocument/publishDiagnostics",
      "params": { "uri": uri, "version": version, "diagnostics": diagnostics },
    }));
  }

  /// The names of the functions declared in `text`, with their ranges.
  fn functions(&self, text: &str) -> Vec<(String, Value)> {
    text
      .match_indices("fn ")
      .filter_map(|(offset, _)| {
        let start = offset + 3;
        let length = text[start..].find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))?;
        (length > 0).then(|| {
          let name = text[start..start + length].to_owned();
          let range = json!({ "start": self.position(text, start), "end": self.position(text, start + length) });
          (name, range)
        })
      })
      .collect()
  }

  fn word_at(&self, params: &Value) -> (String, String) {
    let uri = params["textDocument"]["uri"]
      .as_str()
      .unwrap_or_default()
      .to_owned();
    let text = self.documents.get(&uri).cloned().unwrap_or_default();
    let offset = self.offset(&text, &params["position"]);
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let start = text[..offset]
      .char_indices()
      .rev()
      .find(|&(_, ch)| !is_word(ch))
      .map_or(0, |(index, ch)| index + ch.len_utf8());
    let end = text[offset..]
      .char_indices()
      .find(|&(_, ch)| !is_word(ch))
      .map_or(text.len(), |(index, _)| offset + index);
    (uri, text[start..end].to_owned())
  }

  fn range(&self, text: &str, range: std::ops::Range<usize>) -> Value {
    json!({ "start": self.position(text, range.start), "end": self.position(text, range.end) })
  }

  /// Edits removing the spaces at the ends of lines `lines` of `text`.
  fn trailing_spaces(&self, text: &str, lines: std::ops::Range<usize>) -> Vec<Value> {
    let mut edits = Vec::new();
    let mut start = 0;
    for (index, line) in text.split('\n').enumerate() {
      let content = line.strip_suffix('\r').unwrap_or(line);
      let trimmed = content.trim_end_matches(' ');
      if lines.contains(&index) && trimmed.len() < content.len() {
        let range = self.range(text, start + trimmed.len()..start + content.len());
        edits.push(json!({ "range": range, "newText": "" }));
      }
      start += line.len() + 1;
    }
    edits
  }

  /// The quick fix for the diagnostic at `range` in `uri`, with its edit if
  /// `resolved`.
  fn fix(&self, uri: &str, range: &Value, resolved: bool) -> Value {
    let mut action = json!({
      "title": "Replace with 'fixed'",
      "kind": "quickfix",
      "isPreferred": true,
      "data": { "uri": uri, "range": range },
    });
    if resolved {
      action["edit"] = json!({ "changes": { uri: [{ "range": range, "newText": "fixed" }] } });
    }
    action
  }

  fn handle(&mut self, message: Value) {
    let method = message["method"].as_str().unwrap_or_default().to_owned();
    let params = message["params"].clone();
    let id = message.get("id").cloned();
    // The answer to the configuration request a change set off.
    if method.is_empty() && id == Some(json!("config-2")) {
      self.send(json!({
        "jsonrpc": "2.0", "method": "fake/configured", "params": message["result"],
      }));
      return;
    }
    // The client's answer to `workspace/applyEdit`, which answers the command.
    if method.is_empty() && id == Some(json!("apply-1")) {
      if let Some(command) = self.executing.take() {
        let answer = message.get("result").cloned().unwrap_or(Value::Null);
        self.send(json!({ "jsonrpc": "2.0", "id": command, "result": answer }));
      }
      return;
    }
    let result = match method.as_str() {
      "initialized" => {
        self.send(json!({
          "jsonrpc": "2.0", "id": "watch-1", "method": "client/registerCapability",
          "params": { "registrations": [{
            "id": "watched", "method": "workspace/didChangeWatchedFiles",
            "registerOptions": { "watchers": [{ "globPattern": "**/*.watched" }] },
          }]},
        }));
        return;
      }
      "workspace/didChangeConfiguration" => {
        self.send(json!({
          "jsonrpc": "2.0", "id": "config-2", "method": "workspace/configuration",
          "params": { "items": [{ "section": "fake" }] },
        }));
        return;
      }
      "workspace/didChangeWatchedFiles" => {
        self.send(json!({ "jsonrpc": "2.0", "method": "fake/watched", "params": params }));
        return;
      }
      "initialize" => {
        // Exercise the client's handling of server-to-client requests.
        self.send(json!({
          "jsonrpc": "2.0", "id": "config-1", "method": "workspace/configuration",
          "params": { "items": [{ "section": "fake" }] },
        }));
        json!({
          "capabilities": {
            "positionEncoding": if self.utf16 { "utf-16" } else { "utf-8" },
            "textDocumentSync": { "openClose": true, "change": 2, "save": { "includeText": false } },
            "hoverProvider": true,
            "definitionProvider": true,
            "referencesProvider": true,
            "documentSymbolProvider": true,
            "workspaceSymbolProvider": true,
            "completionProvider": { "triggerCharacters": ["."] },
            "documentFormattingProvider": true,
            "documentRangeFormattingProvider": true,
            "renameProvider": { "prepareProvider": true },
            "codeActionProvider": { "resolveProvider": true },
            "executeCommandProvider": { "commands": ["fake.apply"] },
            "signatureHelpProvider": { "triggerCharacters": ["("], "retriggerCharacters": [","] },
          },
          "serverInfo": { "name": "fake", "version": "1" },
        })
      }
      "textDocument/didOpen" => {
        let uri = params["textDocument"]["uri"]
          .as_str()
          .unwrap_or_default()
          .to_owned();
        let text = params["textDocument"]["text"]
          .as_str()
          .unwrap_or_default()
          .to_owned();
        self.documents.insert(uri.clone(), text);
        self
          .versions
          .insert(uri.clone(), params["textDocument"]["version"].clone());
        self.publish(&uri, Some(&params["textDocument"]["version"]));
        return;
      }
      "textDocument/didChange" => {
        let uri = params["textDocument"]["uri"]
          .as_str()
          .unwrap_or_default()
          .to_owned();
        let mut text = self.documents.get(&uri).cloned().unwrap_or_default();
        for change in params["contentChanges"].as_array().into_iter().flatten() {
          let new_text = change["text"].as_str().unwrap_or_default();
          match change.get("range") {
            Some(range) => {
              let start = self.offset(&text, &range["start"]);
              let end = self.offset(&text, &range["end"]);
              text.replace_range(start..end, new_text);
            }
            None => text = new_text.to_owned(),
          }
        }
        self.documents.insert(uri.clone(), text);
        self
          .versions
          .insert(uri.clone(), params["textDocument"]["version"].clone());
        self.publish(&uri, Some(&params["textDocument"]["version"]));
        return;
      }
      "textDocument/didClose" => {
        let uri = params["textDocument"]["uri"]
          .as_str()
          .unwrap_or_default()
          .to_owned();
        self.documents.remove(&uri);
        return;
      }
      "textDocument/hover" => {
        if self.crash_on_hover {
          std::process::exit(3);
        }
        let (_, word) = self.word_at(&params);
        json!({ "contents": { "kind": "markdown", "value": format!("word `{word}`") } })
      }
      "textDocument/definition" => {
        let (uri, word) = self.word_at(&params);
        let text = self.documents.get(&uri).cloned().unwrap_or_default();
        match text.find(&word) {
          Some(offset) if !word.is_empty() => json!({
            "uri": uri,
            "range": { "start": self.position(&text, offset), "end": self.position(&text, offset + word.len()) },
          }),
          _ => Value::Null,
        }
      }
      "textDocument/references" => {
        let (uri, word) = self.word_at(&params);
        let text = self.documents.get(&uri).cloned().unwrap_or_default();
        let locations: Vec<Value> = if word.is_empty() {
          Vec::new()
        } else {
          text
            .match_indices(&word)
            .map(|(offset, _)| {
              json!({
                "uri": uri,
                "range": { "start": self.position(&text, offset), "end": self.position(&text, offset + word.len()) },
              })
            })
            .collect()
        };
        json!(locations)
      }
      "textDocument/documentSymbol" => {
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
        let text = self.documents.get(uri).cloned().unwrap_or_default();
        let symbols: Vec<Value> = self
          .functions(&text)
          .into_iter()
          .map(|(name, range)| {
            json!({ "name": name, "kind": 12, "range": range, "selectionRange": range })
          })
          .collect();
        json!(symbols)
      }
      "workspace/symbol" => {
        let query = params["query"].as_str().unwrap_or_default();
        let mut symbols = Vec::new();
        for (uri, text) in &self.documents {
          for (name, range) in self.functions(text) {
            if name.contains(query) {
              symbols.push(
                json!({ "name": name, "kind": 12, "location": { "uri": uri, "range": range } }),
              );
            }
          }
        }
        json!(symbols)
      }
      "textDocument/completion" => json!({
        "isIncomplete": false,
        "items": [{ "label": "alpha", "kind": 3 }, { "label": "beta", "kind": 6, "detail": "a variable" }],
      }),
      "textDocument/formatting" | "textDocument/rangeFormatting" => {
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
        let text = self.documents.get(uri).cloned().unwrap_or_default();
        let lines = match params.get("range") {
          Some(range) => {
            let first = range["start"]["line"].as_u64().unwrap_or(0) as usize;
            first..range["end"]["line"].as_u64().unwrap_or(0) as usize + 1
          }
          None => 0..usize::MAX,
        };
        json!(self.trailing_spaces(&text, lines))
      }
      "textDocument/prepareRename" | "textDocument/rename" => {
        let (uri, word) = self.word_at(&params);
        let text = self.documents.get(&uri).cloned().unwrap_or_default();
        if word.is_empty() || word.chars().all(|ch| ch.is_ascii_digit()) {
          let message = "You cannot rename this element.";
          if let Some(id) = id {
            self.send(json!({
              "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": message },
            }));
          }
          return;
        }
        let ranges: Vec<std::ops::Range<usize>> = text
          .match_indices(&word)
          .map(|(offset, _)| offset..offset + word.len())
          .collect();
        if method == "textDocument/prepareRename" {
          let offset = self.offset(&text, &params["position"]);
          let range = ranges
            .iter()
            .find(|range| range.start <= offset && offset <= range.end)
            .cloned()
            .unwrap_or_default();
          json!({ "range": self.range(&text, range), "placeholder": word })
        } else {
          let new_name = params["newName"].as_str().unwrap_or_default();
          let edits: Vec<Value> = ranges
            .into_iter()
            .map(|range| json!({ "range": self.range(&text, range), "newText": new_name }))
            .collect();
          json!({ "documentChanges": [{
            "textDocument": { "uri": uri, "version": self.versions.get(&uri) },
            "edits": edits,
          }]})
        }
      }
      "textDocument/codeAction" => {
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
        let mut actions: Vec<Value> = params["context"]["diagnostics"]
          .as_array()
          .into_iter()
          .flatten()
          .filter(|diagnostic| diagnostic["message"] == "found error")
          .map(|diagnostic| self.fix(uri, &diagnostic["range"], false))
          .collect();
        actions.push(json!({
          "title": "Apply by Command",
          "command": "fake.apply",
          "arguments": [uri],
        }));
        json!(actions)
      }
      "codeAction/resolve" => {
        let uri = params["data"]["uri"].as_str().unwrap_or_default();
        self.fix(uri, &params["data"]["range"], true)
      }
      "workspace/executeCommand" => {
        let uri = params["arguments"][0].clone();
        let start = json!({ "line": 0, "character": 0 });
        self.executing = id;
        self.send(json!({
          "jsonrpc": "2.0", "id": "apply-1", "method": "workspace/applyEdit",
          "params": {
            "label": "Apply by Command",
            "edit": { "changes": { uri.as_str().unwrap_or_default(): [
              { "range": { "start": start, "end": start }, "newText": "applied\n" },
            ]}},
          },
        }));
        return;
      }
      "textDocument/signatureHelp" => {
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
        let text = self.documents.get(uri).cloned().unwrap_or_default();
        let offset = self.offset(&text, &params["position"]);
        match text[..offset].rfind('(') {
          Some(open) if !text[open..offset].contains(')') => {
            let commas = text[open..offset].matches(',').count();
            json!({
              "signatures": [{
                "label": "f(a, b)",
                "documentation": { "kind": "markdown", "value": "Adds `a` and `b`." },
                "parameters": [{ "label": [2, 3] }, { "label": "b" }],
              }],
              "activeSignature": 0,
              "activeParameter": commas,
            })
          }
          _ => Value::Null,
        }
      }
      "fake/text" => {
        let uri = params["uri"].as_str().unwrap_or_default();
        json!(self.documents.get(uri))
      }
      "fake/sleep" => {
        std::thread::sleep(Duration::from_millis(params["ms"].as_u64().unwrap_or(0)));
        Value::Null
      }
      "shutdown" => Value::Null,
      "exit" if self.ignore_exit => return,
      "exit" => std::process::exit(0),
      _ => {
        // Responses to our own requests have no method and need no answer.
        if let Some(id) = id
          && message.get("method").is_some()
        {
          self.send(json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32601, "message": format!("{method} not found") },
          }));
        }
        return;
      }
    };
    if let Some(id) = id {
      self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }
  }
}

fn main() {
  let args: Vec<String> = std::env::args().collect();
  let flag = |name: &str| args.iter().any(|arg| arg == name);
  // The process `--stubborn` starts: it outlives any test that fails.
  if flag("--linger") {
    std::thread::sleep(Duration::from_secs(60));
    return;
  }
  let stubborn = flag("--stubborn");
  let lingering = stubborn.then(|| {
    std::process::Command::new(std::env::current_exe().expect("own path"))
      .arg("--linger")
      .stdin(std::process::Stdio::null())
      .spawn()
      .expect("starts")
  });
  let mut server = Server {
    utf16: flag("--utf16"),
    crash_on_hover: flag("--crash-on-hover"),
    ignore_exit: flag("--ignore-exit") || stubborn,
    documents: HashMap::new(),
    versions: HashMap::new(),
    executing: None,
    out: std::io::stdout(),
  };
  let mut input = BufReader::new(std::io::stdin());
  while let Ok(Some(body)) = read_message(&mut input) {
    if let Ok(message) = serde_json::from_slice::<Value>(&body) {
      server.handle(message);
    }
  }
  if let Some(mut lingering) = lingering {
    let _ = lingering.wait();
  }
}
