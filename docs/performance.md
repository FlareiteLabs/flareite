# Performance

This document describes how Flareite's performance is measured, the baseline
measured at Milestone 4, and how future optimizations should be chosen. The
reasoning behind the architecture is in
[ADR 0004](adr/0004-performance-foundations.md).

All numbers below are single-machine measurements. They are a baseline to
compare against, not claims about other machines or other editors.

## How to measure

| What                                           | Command                                                                                           |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| Microbenchmarks (text, core, files, workspace) | `pnpm bench` (Criterion; reports in `target/criterion/`)                                          |
| One suite or benchmark                         | `cargo bench -p flareite-benches --bench workspace -- search`                                     |
| Compare with a saved baseline                  | `cargo bench -p flareite-benches -- --save-baseline m4`, later `-- --baseline m4`                 |
| End-to-end: startup and idle memory            | `pnpm tauri build --no-bundle`, then `pnpm bench:app startup`                                     |
| End-to-end: typing latency                     | `pnpm bench:app typing`                                                                           |
| End-to-end: typing in a source file            | `pnpm bench:app typing --file path/to/file.rs --language extensions/test-fixtures/languages/rust` |
| End-to-end, with your extensions loaded        | add `--extensions` (runs skip extensions otherwise)                                               |
| End-to-end: large-document scrolling           | `pnpm bench:app scroll`                                                                           |
| End-to-end: fast scrolling (blank frames)      | `pnpm bench:app fling` (add `--file` and `--language` to scroll a highlighted file)               |
| End-to-end: dragging the minimap               | `pnpm bench:app drag` (4,000 lines, or `--file`)                                                  |
| End-to-end: opening a file                     | `pnpm bench:app open --file path/to/file`                                                         |
| Live statistics while using the app            | Command palette → **Developer: Toggle Performance Overlay**                                       |

The end-to-end harness (`scripts/bench.mjs`) launches the release build with
`--bench <name>`. The frontend drives the real editor (IPC, React rendering,
DOM measurement, and painting included) and reports its timings. While the
app waits to exit, the harness measures the memory of the whole process tree:
the app plus its WebView processes. Benchmark runs use an empty, ephemeral
session and never touch your saved state. Results are written to
`target/bench/results.json`.

### Fixtures

Inputs are generated deterministically (`benches/src/lib.rs`), not checked
in:

- **Code-like text:** indented lines of 40–100 characters of keywords and
  punctuation.
- **Synthetic workspace:** 10,000 source files of about 2 KiB in a nested
  layout, plus a `.gitignore`d `target/` folder that searches must skip.
- **Synthetic path lists:** 100,000 paths for Go to File.
- **Large documents** for the end-to-end benchmarks: generated in the app
  (`bench_generate`), 200,000 lines of 80 columns (about 15.5 MB).

### Things that skew results

- **Overlapping launches.** Back-to-back startups while the previous
  instance's WebView processes are still exiting roughly double startup time;
  the harness pauses 3 s between runs (`--pause`).
- **Antivirus.** Real-time scanning (Windows Defender) adds a large per-file
  cost to opening files, which dominates find in files over many small files.
- **Display refresh rate.** Keystroke-to-paint latency can't be lower than the
  wait for the next frame. The development machine runs at 75 Hz (13.3 ms
  frames).
- **Cold starts.** The OS file cache is warm after the first run. A true cold
  start (after a reboot) isn't automated; the first run of a session is
  reported separately as the closest approximation.

### Regression checks

Since Milestone 8, three layers guard against regressions:

1. **Budgets that count work** run with the tests, in CI on every platform. Timing on shared CI
   runners is too noisy to compare, but the amount of work isn't. A keystroke in a
   200,000-line document must send exactly one line and less than 1 KB, and a page of
   scrolling at most a page of lines
   (`large_documents_cost_the_same_per_keystroke_as_small_ones` in
   `src-tauri/src/editor/protocol.rs`). The core's view tests check the same for incremental
   updates.
2. **End-to-end comparison** with a committed baseline, on the reference machine:

   ```sh
   pnpm tauri build --no-bundle
   pnpm bench:app all
   pnpm bench:check                       # node scripts/perf-check.mjs
   node scripts/perf-check.mjs --update   # accept the current results as the baseline
   ```

   `benches/baseline.json` holds the metrics and the machine they were measured on. Each kind
   of metric has its own tolerance:

   | Metric kind                     | Fails when it grows by more than |
   | ------------------------------- | -------------------------------- |
   | Paint latency (keystroke, page) | 10% + 2 ms (less than a frame)   |
   | Other timings (startup, open)   | 25% + 5 ms                       |
   | Backend `editor.apply`          | 50% + 0.1 ms                     |
   | Memory                          | 15% + 8 MB                       |
   | Binary size                     | 10%                              |

   Results from a different CPU or OS than the baseline's are compared with a warning.

3. **Microbenchmarks** against a saved Criterion baseline:

   ```sh
   cargo bench -p flareite-benches -- --save-baseline before
   cargo bench -p flareite-benches -- --baseline before
   node scripts/perf-check.mjs micro --threshold 10
   ```

   A benchmark fails only when even the lower end of Criterion's confidence interval for the
   change is above the threshold, so noise doesn't fail it.

## Baseline (Milestone 4)

Development machine: AMD Ryzen 7 5800X (8 cores, 16 threads), 32 GB RAM,
Windows 11 (build 26200), 75 Hz display. Release build, Rust 1.99,
WebView2 155.

### Startup and memory

| Measurement                                   | Result                          |
| --------------------------------------------- | ------------------------------- |
| Launch → frontend ready, first run            | 403 ms                          |
| Launch → frontend ready, 5 later runs         | 401 ms median (363–472 ms)      |
| Idle memory, whole process tree (7 processes) | 389 MB resident, 193 MB private |
| Idle memory, Flareite backend process         | 34 MB resident                  |
| Backend with a 15.5 MB document open          | 67 MB resident                  |
| Backend with a 100 MB document open           | 149 MB resident                 |

Most idle memory belongs to WebView2 (browser, GPU, renderer, and utility
processes). "Resident" double-counts pages shared between those processes;
"private" is closer to what Flareite costs the system. Memory with extensions
is measured once extensions exist (M6).

### Editing and rendering (end to end)

| Measurement                                             | p50     | p95     | max     |
| ------------------------------------------------------- | ------- | ------- | ------- |
| Keystroke → painted, small document                     | 13.3 ms | 13.7 ms | 26.8 ms |
| Keystroke → painted, 200,000-line document              | 13.3 ms | 13.7 ms | 19.8 ms |
| IPC round trip (request → view received)                | 2.0 ms  | 6.1 ms  | 21.8 ms |
| Backend `editor_apply` (apply the edit, build the view) | 0.10 ms | 0.26 ms | 1.5 ms  |
| Frontend render and measure                             | 0.4 ms  | 0.6 ms  | 3.3 ms  |
| Lines transferred per keystroke                         | 1       | 1       | —       |
| Page jump (~1,000 lines) → painted, 200,000 lines       | 26.6 ms | 28.0 ms | 36.4 ms |
| Lines transferred per page jump                         | 78      | 78      | —       |

Other end-to-end measurements:

- A 300-character typing burst (dispatched without waiting) settles in
  13.4 ms: the session batches it into a single request.
- Jumping to the end of a 200,000-line document paints in 13.0 ms.
- Opening a 100 MB file takes 90 ms to load and 111 ms to the first paint.

Keystroke latency is one frame: the work (≈2.5 ms) finishes well before the
next display refresh, so the measurement is the frame wait. A page jump is two
frames: the browser delivers the scroll event one frame after the scroll
position changes, then paints the new lines on the next. Ordinary wheel
scrolling stays within the 20 lines fetched above and below the viewport, so
it needs no request.

### Microbenchmarks (Criterion)

Median of Criterion's estimate.

| Benchmark                                                      | Time                          |
| -------------------------------------------------------------- | ----------------------------- |
| Insert + delete one character, 1 MB document                   | 1.7 µs                        |
| Insert + delete one character, 100 MB document                 | 2.4 µs                        |
| Offset → UTF-16 position, 100 MB                               | 0.36 µs                       |
| UTF-16 position → offset, 100 MB                               | 0.83 µs                       |
| Line changes since a version (100 edits)                       | 0.61 µs                       |
| Build a buffer from 10 MB / 100 MB of text                     | 3.9 ms / 66 ms                |
| Type 1,000 characters (core), empty document                   | 3.6 ms                        |
| Type 1,000 characters (core), 10 MB document                   | 1.8 ms                        |
| Undo and redo 1,000 groups                                     | 4.7 ms                        |
| Viewport update, 80 lines of a 50 MB document, full            | 46 µs                         |
| Viewport update after one keystroke (incremental)              | 0.81 µs                       |
| Find all, 10 MB, common literal                                | 1.0 ms                        |
| Find all, 10 MB, rare literal                                  | 1.9 ms                        |
| Find all, 10 MB, regex `\bindex\.\w+`                          | 2.5 ms                        |
| Replace all of a common word in 1 MB                           | 8.7 ms                        |
| Open (read, validate, build) 1 / 10 / 100 MB                   | 0.77 / 7.4 / 89 ms            |
| Save (atomic, with fsync) 1 / 10 MB                            | 17 / 53 ms                    |
| Discover and index 10,000 files                                | 218 ms                        |
| Find in 10,000 files, rare literal                             | 285 ms (982 ms on one thread) |
| Find in 10,000 files, regex `fn\s+\w+\(`                       | 291 ms (996 ms on one thread) |
| Find in 10,000 files, common literal (stops at 20,000 matches) | 51 ms (140 ms on one thread)  |
| List a directory of 100 entries                                | 2.4 ms                        |
| Go to File over 100,000 paths                                  | 5–9 ms                        |

## Milestone 5: syntax highlighting and language servers

Same machine and method. On the measurement day, the Milestone 4 binary
itself measured 14.5 ms p50 keystroke-to-paint (against 13.3 ms when the
baseline was recorded), so the frame timing of the environment had shifted.
The comparison below therefore runs both builds back to back.

| Measurement (release builds, same session) | Milestone 4          | Milestone 5      |
| ------------------------------------------ | -------------------- | ---------------- |
| Keystroke → painted, small plain document  | 14.5 ms p50 (2 runs) | 14.5–14.6 ms p50 |
| Keystroke → painted, 200,000-line document | 14.5 ms p50          | 14.5 ms p50      |
| Startup, later runs (median of 3)          | 982 ms (381–1,018)   | 349 ms           |
| Idle memory, whole process tree            | 366 MB resident      | 365 MB resident  |
| Binary size (`flareite.exe`)               | 15.2 MB              | 21.7 MB          |

Grammars and highlight queries are compiled on first use, so they cost
nothing at startup; the binary grows by the six bundled grammars. p95 values
varied between 15 and 32 ms from run to run in both builds, so they are not
compared.

Typing in the middle of a 676-line Rust file (a copy of
`crates/flareite-text/src/buffer.rs` in a small Cargo project):

| Measurement                                                  | p50     | p95     | max     |
| ------------------------------------------------------------ | ------- | ------- | ------- |
| Keystroke → painted, highlighted, no language server (`.py`) | 14.5 ms | 15.4 ms | 57.8 ms |
| Keystroke → painted, with rust-analyzer idle                 | 14.6 ms | 18.8 ms | 189 ms  |
| Keystroke → painted, while rust-analyzer indexes             | 14.6 ms | 35.2 ms | 191 ms  |
| Backend highlighting per view (`syntax.highlight`)           | 0.95 ms | 1.9 ms  | 3.1 ms  |
| Backend `editor_apply`, including sync and highlighting      | 0.17 ms | 1.9 ms  | 3.1 ms  |
| First (background) parse, including compiling the query      | 22 ms   | —       | —       |

- Highlighting stays within one frame: typing in a highlighted file
  measures the same as plain text. Incremental reparses finished inline;
  the only background parse was the first one.
- With rust-analyzer, the backend work per keystroke is still a few
  milliseconds (sending the change is a queued write). The wider tail
  comes from CPU contention: while rust-analyzer indexes the standard
  library it uses every core, and frames are delayed even though Flareite's
  own timings don't change. The benchmark therefore waits until the server
  reports it is idle before measuring (`languageServerSettled` in
  `src/bench/runner.ts`).
- Language servers dominate memory: with rust-analyzer indexed, the
  process tree used about 2 GB resident, against about 90 MB for Flareite's
  own process.

## Milestone 6: WebAssembly extensions

Same machine and method, measured on 2026-10-24. Both builds ran back to back on the same day,
because frame timing shifts between days:

- **Previous build:** Milestone 5 plus the Ctrl+hover change (commit `9228323`).
- **Milestone 6:** with and without the example extension, Tidy, active.

For the Tidy runs, Tidy was loaded as a development extension with both permissions granted. It
reads every open document (including the 200,000-line benchmark document) and republishes
diagnostics after every change.

| Measurement                                   | Previous build  | Milestone 6         | Milestone 6 + Tidy |
| --------------------------------------------- | --------------- | ------------------- | ------------------ |
| Startup, later runs (median of 4, two series) | 454 / 465 ms    | 453 / 407 ms        | 468 ms             |
| Idle memory, whole process tree               | 358–361 MB      | 356–359 MB          | 371 MB             |
| Idle memory, app process                      | 33 MB           | 31–33 MB            | 44 MB              |
| Binary size (`flareite.exe`)                  | 20.7 MB         | 37.3 MB             | —                  |
| Keystroke → painted, small document           | 13.3 ms p50     | 13.3 ms p50         | 13.3 ms p50        |
| Keystroke → painted, p95                      | 14.0–14.1 ms    | 13.9–14.1 ms        | 13.8 ms            |
| Backend `editor_apply` p50 / p95              | 0.115 / 0.35 ms | 0.13 / 0.37–0.40 ms | 0.18 / 0.47 ms     |
| App memory after the typing benchmark         | 82 MB           | 82 MB               | 97 MB              |

- **Startup and idle memory** are unchanged. Startup was also slower than the Milestone 5
  numbers on the measurement day for the previous build itself (454–465 ms against 349 ms), so
  that difference comes from this machine, not from this milestone.
- **Extensions cost nothing until they activate.** Loading Tidy (read, componentize, compile,
  instantiate, activate) took 14–20 ms, on its own thread.
- **Typing stays at one frame** with an extension active. The backend's extra work per keystroke
  is queueing the change: about 0.01–0.02 ms without extensions and about 0.07 ms with Tidy.
  Tidy's own work runs on its thread.
- **Extra views.** Each diagnostics update makes the frontend refresh the active editor, so with
  Tidy every keystroke is followed by one extra `editor_view` (p50 0.10 ms, p95 0.25 ms; 948
  in the run against 5 without Tidy). That is the main cost of an extension that republishes
  diagnostics on every change.
- **Binary size** grew by 16.6 MB, almost all Wasmtime's Cranelift compiler. Thin LTO or
  caching compiled extensions are options if size matters; neither is enabled.

## Milestone 7: workbench contributions

A spot check on 2026-10-24, without extensions loaded. Startup was slower on the machine that
evening for every build, so the previous build (Milestone 5 plus the Ctrl+hover change) was run
back to back again.

| Measurement                         | Previous build | Milestone 7           |
| ----------------------------------- | -------------- | --------------------- |
| Startup, later runs (median of 4)   | 705 / 671 ms   | 663 / 645 ms          |
| Keystroke → painted, small document | —              | 13.3 ms p50, 14.1 p95 |
| Keystroke → painted, 200,000 lines  | —              | 13.4 ms p50, 14.2 p95 |
| Idle memory, whole process tree     | —              | 363 MB resident       |
| Binary size                         | 20.7 MB        | 37.9 MB (M6: 37.3)    |

- **Without extensions, nothing changes.** Contributions add work only when an extension uses
  them. Views without decorations carry an empty list, and extension commands and keybindings
  are registered once per extension list change.
- **Decorations.** Decorated lines are re-rendered when a view changes (their text runs are
  rebuilt); undecorated lines aren't.
- **Status and view updates.** These send an event only when the content actually changed. The
  TODOs example republishes its view after every change, but the frontend refetches only when
  something differs.

## Milestone 8: hardening

Measured on 2026-10-25, without extensions. The Milestone 7 build was rebuilt from its commit and
run alternately with Milestone 8, because startup varied between 600 and 880 ms on the machine
that evening for both builds.

| Measurement                         | Milestone 7              | Milestone 8              |
| ----------------------------------- | ------------------------ | ------------------------ |
| Startup, later runs (median of 4)   | 859 / 662 ms             | 761 / 691 ms             |
| Idle memory, whole process tree     | 356 / 359 MB             | 356 / 359 MB             |
| Keystroke → painted, small document | 13.3 ms p50, 13.9 ms p95 | 13.3 ms p50, 14.0 ms p95 |
| Keystroke → painted, 200,000 lines  | 13.4 ms p50, 13.9 ms p95 | 13.3 ms p50, 13.9 ms p95 |
| Page jump → painted (p50)           | 26.7 ms                  | 26.7 ms                  |
| Generate + open 200,000 lines       | 87–102 ms (3 runs)       | 89–107 ms (3 runs)       |
| Binary size                         | 37.9 MB                  | 38.1 MB                  |

The baseline in `benches/baseline.json` comes from a separate full run (startup 638 ms median).
A second, independent run passed `pnpm bench:check` with every metric within 3% except
generate + open, which was 11% faster.

- **Typing costs the same.** Each view now also refreshes the input mirror for screen readers
  (one line of text and a selection, set only when changed) and signals the backup thread (an
  atomic flag). Neither shows in keystroke latency.
- **Backups.** These run on their own thread, outside the editor lock except for O(1) snapshots.
  They are off in benchmark runs, which keep no session state.
- **Extension accounting** costs one clock reading per call into an extension.
- **Generate + open** measured 61 ms in an earlier session. Both builds measured 87–107 ms this
  evening, so the difference is the machine, not this milestone.
- **Open item.** The Criterion regex find benchmark (`core/find_10MB/regex_word`) measured
  4.3 ms this evening, against 2.5 ms in the Milestone 4 table, while the literal searches are
  about 20% slower than then. Milestone 8 didn't change search, but no Milestone 4 baseline was
  saved to compare against. Saving Criterion baselines per milestone (`--save-baseline m8`)
  makes such drift attributable from now on.

## Minimap

Measured on 2026-10-25 with a release build ([ADR 0010](adr/0010-minimap.md)). The document was
a 200,000-line Rust file of 10 MB, so it was syntax highlighted. Each run paged through the file
40 times in steps of 2,500 lines, then typed 10 characters. The app was restarted between runs
because `perf_stats` can't be reset.

| Backend timing (`perf_stats`)         | Minimap shown            | Minimap hidden  |
| ------------------------------------- | ------------------------ | --------------- |
| `editor.apply` (keystroke)            | 1.54 ms p50, 4.1 ms p95  | 1.54 ms, 4.3 ms |
| `editor.view` (scroll)                | 1.44 ms p50, 2.1 ms p95  | 1.45 ms, 2.8 ms |
| `editor.minimap` (≈570 lines, worker) | 11.7 ms p50, 14.0 ms p95 | —               |
| `editor.minimap.lock` (snapshot)      | 6 µs p50, 15 µs max      | —               |

- **Editing doesn't wait for it.** The minimap's work runs with the editor lock released, so
  keystrokes and scrolling cost the same either way. Most of their time in this file goes to
  syntax highlighting (`syntax.highlight`, 1.4 ms p50 in both runs).
- **Highlighting dominates.** About 20 µs per line, the same rate as the editor's view.
- **Under the lock first.** The first version built the minimap under the editor lock, at
  13.6 ms p50 and 60 ms p95 per request in a debug build. That held up edits, which is why it
  now snapshots instead.

## Fast scrolling

Measured on 2026-11-20 with release builds ([ADR 0055](adr/0055-keeping-up-with-fast-scrolling.md)).
Two benchmarks check, frame by frame, what the editor paints: how much of its visible area has
no lines, whether the minimap's last drawing left rows blank, whether rows drawn from the copy of
the lines sit exactly where their lines belong (none ever didn't), and how long frames take (long
frames are lag).

- `pnpm bench:app drag` drags the minimap's slider from the top of a 4,000-line document to the
  bottom and back, in 20 frames each way (about 200 lines a frame) and in 60, with mouse events
  through the minimap's own handlers.
- `pnpm bench:app fling` scrolls a large document by a fixed number of lines every frame, down for
  120 frames and back up, as a fast wheel or trackpad fling does. At 75 Hz, 40 lines a frame is
  3,000 lines a second.

They ran on generated documents and on Rust files highlighted with the test fixture's grammar:
this repository's Rust sources concatenated (83,000 lines), and its first 4,000 lines for the
drag.

**Dragging the minimap,** 4,000-line Rust file. "Before" is the first attempt of ADR 0055 (rows
ahead in views), the build in which the problem was reported; the drag benchmark didn't exist
before it.

| Top to bottom in 20 frames    | Before     | After            |
| ----------------------------- | ---------- | ---------------- |
| Frames with the editor blank  | 65%        | 0%               |
| Frames with the minimap blank | 43%        | 0%               |
| Frame time p50 / p95          | 20 / 48 ms | 13–16 / 19–30 ms |

In 60 frames the first attempt was blank in 3% of frames with a p95 of 24 ms; now 0%, 18–21 ms.
The generated document measured 0% blank with a p95 of 15–20 ms.

**Flinging,** frames with the editor partly blank (share of the area in parentheses). "Before" is
the build before ADR 0055.

| Lines a frame | Before     | After     |
| ------------- | ---------- | --------- |
| 10            | 66% (35%)  | 0% (0%)   |
| 40            | 100% (99%) | 2% (0.7%) |
| 100           | 100% (99%) | 2% (1.7%) |

The remaining blank frames are the first of each fling, which starts at full speed from a
standstill. The minimap was blank in up to 20% of frames before, and in at most 3% (100 lines a
frame) after. Frame times stay near the display's: p95 15–21 ms in the Rust file.

- **Typing** costs the same: keystroke to paint p50 13.3 ms and p95 13.8–14.2 ms, in the small,
  the 200,000-line, and the Rust document, as before.
- **Page jumps** cost the same: p50 26.7 ms, p95 28.4 ms (27.3–27.9 ms before).
- **Backend.** Highlighting only the lines a view brings in keeps scroll views cheap. The copy of
  the lines and the minimap's whole-document rows are fetched on worker threads.

## Languages from extensions

Since [ADR 0014](adr/0014-languages-only-from-extensions.md), every grammar is a
WebAssembly module run by Tree-sitter's Wasmtime store instead of native code.
Before removing the native grammars, both were compared on the same text in one
release build (best of 15 runs, on the 1,370-line
`crates/flareite-extension-host/src/host.rs`):

| Measurement                             | Native | WebAssembly |
| --------------------------------------- | ------ | ----------- |
| Full parse                              | 5.2 ms | 7.7 ms      |
| Incremental reparse after one keystroke | 144 µs | 164 µs      |
| Highlighting 60 lines (query, no parse) | 229 µs | 225 µs      |

Highlighting doesn't change, as queries run natively either way, and a
keystroke's reparse stays far inside the 3 ms inline budget. First parses of
large files took longer and go to the background as before. Highlight output
was identical for every former built-in language on real files from this
repository.

## Optimizations made from these measurements

1. **Parallel find in files.** On one thread, searching 10,000 small files took
   about 1 s, although searching the same 20 MB in memory takes about 4 ms (extrapolated
   from the 10 MB benchmark) and
   walking the tree takes 218 ms. The time went to opening and reading files
   one at a time. Searching with up to four walker threads cut it to 285 ms
   (3.4×).
2. **Incremental views.** The backend now sends only lines the frontend lacks:
   one line per keystroke instead of the whole viewport (about 80 lines), and
   0.8 µs instead of 46 µs to build.
3. **Background match counting.** Counting every find match in a large
   document would scan it on every keystroke. Documents over 1 MiB show
   visible matches at once and count on the worker pool.
4. **Cheap extension sync.** Every view syncs the document with the extension
   host. The first version rebuilt the document's description and change list
   each time. Now views that change nothing return early, and change lists
   are built only when a running extension reads documents. Without
   extensions, the remaining cost is 0.01–0.02 ms per keystroke.

One hypothesis turned out to be wrong. Removing an extra animation-frame wait
before fetching lines on scroll didn't change page-jump latency; the two
frames come from scroll event delivery, as described above. The simpler code
was kept.

## Strategy for further optimization

- **Measure first.** Add or extend a benchmark that shows the problem, save a
  Criterion baseline, change one thing, and compare.
- **Latency budget.** Typing must stay within one frame end to end. Today about
  2.5 ms of the 13.3 ms frame is used, mostly by IPC. Features that run on every
  keystroke (syntax highlighting in M5, extension notifications in M6) get the
  remaining budget only if they're incremental. Anything else goes on the
  worker pool with keyed replacement.
- **Watch these numbers as features arrive:**
  - IPC round-trip time, since it's the largest share of a keystroke;
  - backend `editor.apply` p95;
  - process memory with large documents (the undo history keeps edited text);
  - idle WebView memory.
- **Known opportunities, not yet needed:**
  - a binary or more compact view encoding if IPC grows;
  - saving without `fsync` for very large files (a durability trade-off to
    decide deliberately);
  - horizontal windowing for lines longer than 16 KiB;
  - parallel indexing for very large workspaces.
