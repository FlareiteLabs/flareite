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
