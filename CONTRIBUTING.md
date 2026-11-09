# Contributing to Flareite

This guide is for people working on Flareite itself. To write an extension, see
[`docs/extensions.md`](docs/extensions.md). For how the pieces fit together, see
[`docs/architecture.md`](docs/architecture.md).

## Setting up

Install the tools listed under [Prerequisites](README.md#prerequisites), then:

```sh
pnpm install
rustup target add wasm32-unknown-unknown   # for extensions and the extension host's tests
pnpm tauri dev
```

Platform notes:

- **Windows.** You need the Visual Studio C++ build tools. WebView2 ships with Windows 11. The
  first build downloads the ConPTY that terminals use from nuget.org, so it needs network access
  (see `src-tauri/build.rs`).
- **macOS.** You need the Xcode command line tools.
- **Linux.** You need the WebKitGTK 4.1 development packages. On Debian and Ubuntu:

  ```sh
  sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev
  ```

  Without them, the crates other than the app still build and test:
  `cargo test --workspace --exclude flareite-app --exclude flareite-benches`. That command also
  works in WSL to test Linux behavior from a Windows machine.

## Before sending a change

Run what CI runs (`.github/workflows/ci.yml`):

```sh
pnpm format:check                                      # Prettier and rustfmt
pnpm typecheck
pnpm test                                              # frontend tests, including axe checks
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                                 # builds the WebAssembly test extensions
(cd extensions && cargo clippy --release -- -D warnings)
```

CI runs the Rust checks on Windows, macOS, and Linux. Files use LF line endings
(`.gitattributes`). Rust is indented with two spaces (`rustfmt.toml`).

## Conventions

- **Where code goes.** Text, editing, and file logic belongs in the crates (see the table in the
  architecture document); `src-tauri` only wires them to Tauri. Only `src/ipc/` calls `invoke`.
- **Errors.**
  - Non-test Rust code doesn't `unwrap` (a Clippy lint) or `expect`.
  - Commands return `CommandError`s with a message the user can act on.
  - Problems found in the background go to the user as `notice` events, not just the log.
- **Unsaved work.** Anything that can lose the user's text needs a test. The rope snapshot, save
  point, and backup mechanisms exist so I/O never happens under the editor lock.
- **Decisions.** A decision with lasting consequences gets an ADR in `docs/adr/`. Behavior that
  users or extension authors see goes into the README, the architecture document, or the
  extension guide in the same change.

## Releasing

Releases are built by `.github/workflows/release.yml` when a version tag is pushed:

1. Set the version in `Cargo.toml` (`[workspace.package]`) and `package.json`, and commit.
2. Tag the commit and push the tag: `git tag v0.2.0 && git push origin v0.2.0`. For a prerelease,
   add its name to the tag and leave the versions alone: `v0.2.0-alpha.1` publishes 0.2.0's code
   as a prerelease. Extensions are checked against 0.2.0, which keeps the build able to run those
   that need `>=0.2.0` (semver puts `0.2.0-alpha.1` before `0.2.0`), while the build knows and
   shows its tag (`FLAREITE_RELEASE_TAG`), which is how it tells which releases are newer.
3. The workflow checks that the tag's `major.minor.patch` matches both versions, runs all of CI
   on the commit, drafts a release with notes generated from the commits, builds the installers
   on Windows, macOS (universal), and Linux (on Ubuntu 22.04, for older distributions) and
   uploads them with their update signatures, adds the update manifest (`latest.json`, from
   `scripts/updater-manifest.mjs`), and then publishes the release. Installed copies find it
   there and offer it ([ADR 0040](docs/adr/0040-updating-flareite-from-github-releases.md)).

If a build fails, the draft is left on the releases page with what was uploaded. Delete it and
the tag (`git push --delete origin v0.2.0`), fix the problem, and tag again.

Updates are signed with the updater key. Its public half is `plugins.updater.pubkey` in
`src-tauri/tauri.conf.json`; the private key is the `TAURI_SIGNING_PRIVATE_KEY` repository
secret, with its password in `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (empty for a key without one),
and the workflow stops if it's missing. Keep a copy of the key somewhere safe: installed copies
trust only that key, so a new one means everyone installs the next release by hand. Local builds
aren't signed for updating and need no key. The installers aren't code-signed; that needs
certificates added as repository secrets, which `tauri-action` reads.

## Accessibility

Every new piece of UI should work from the keyboard and with a screen reader:

- **Roles.** Use the native element or ARIA role (button, tree, tablist, menu, dialog), with an
  accessible name.
- **Lists.** Lists navigate with the arrow keys. Popups close with `Escape` and return focus.
- **Menus.** Context menus also open with `Shift+F10` and the menu key.
- **Indicators.** Colors come from theme tokens. An indicator drawn only with a background or a
  shadow also needs a forced-colors rule in `src/styles/accessibility.css`.
- **Messages.** Information may disappear on a timer; warnings and errors must stay until
  dismissed.

Add the new surface to `src/app/accessibility.test.tsx`, which runs axe-core over it.

## Performance

Responsiveness is a requirement, so changes on hot paths need numbers.

- **Budgets in tests.** Some tests count work rather than time it: lines and bytes sent per
  keystroke in a 200,000-line document, lines per page of scrolling. They run in CI on every
  platform. Keep them passing; tighten them when you make something cheaper.
- **Microbenchmarks.** Save a baseline before your change, compare after, and let the script
  judge:

  ```sh
  cargo bench -p flareite-benches -- --save-baseline before
  # ...make the change...
  cargo bench -p flareite-benches -- --baseline before
  node scripts/perf-check.mjs micro --threshold 10
  ```

- **End to end.** Startup, memory, typing latency, and scrolling, against the release build:

  ```sh
  pnpm tauri build --no-bundle
  pnpm bench:app all
  pnpm bench:check          # compares with benches/baseline.json
  ```

  The baseline was measured on one machine. On another, record your own first
  (`node scripts/perf-check.mjs --update`), and don't commit it unless the reference machine
  changes. See [`docs/performance.md`](docs/performance.md) for the numbers and what skews them.

## Testing in the real app

- **Your files.** The app reads and writes your real settings and session state (see
  [Settings](README.md#settings)). Back them up before testing changes that write them.
- **Development extensions.** `extensions.development` in `settings.toml` loads extensions from
  their folders. Those with a `[build]` in their manifest (Tidy and TODOs) are rebuilt and
  reloaded when a file in their folder changes.
- **Remote debugging.** To drive the WebView from scripts on Windows, set
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333` before launching. Then
  connect with the Chrome DevTools Protocol, for example to read the accessibility tree or
  emulate `forced-colors`.

## License

By contributing, you agree that your contributions are licensed under the project's terms: MIT OR
Apache-2.0, at the user's option.
