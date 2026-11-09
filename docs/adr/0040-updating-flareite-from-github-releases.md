# ADR 0040: Updating Flareite from its GitHub releases

- **Status:** Accepted
- **Date:** 2026-11-09

## Context

Extensions update from their GitHub releases
([ADR 0036](0036-installing-extensions-from-github-releases.md)), but Flareite itself had to be
downloaded and installed again by hand for every release. The user asked for Flareite to update
from its releases too, and chose: Tauri's updater with signed updates, a check at startup that a
setting turns off, a channel that follows the running build, and a download that waits for the
user to restart.

Two things made this more than adding the plugin:

- **Every prerelease reports the same version.** `v0.1.0-alpha.1` and `v0.1.0-alpha.2` both build
  as 0.1.0 (the manifests carry only `major.minor.patch`, so extensions that need `>=0.1.0` run in
  them), so a running build can't tell which alpha it is.
- **The updater checks the version against the signature.** Tauri's signer records the version a
  file was built as in the signature's trusted comment, and the updater refuses an update whose
  manifest announces another. Building a prerelease as `0.1.0-alpha.3` isn't possible either: the
  MSI's version must be numeric.

## Decisions

### The app is signed for updating, not for the operating system

The installers carry a minisign signature (`.sig`) made with a key held in the
`TAURI_SIGNING_PRIVATE_KEY` repository secret; the public half is in `tauri.conf.json`. The app
installs only what that key signed. Unlike an extension, Flareite isn't sandboxed, so GitHub's
digest (which proves only that the file is the one the release has) isn't enough: whoever can
publish a release could otherwise run code on every user's machine. The key is separate from code
signing, which still isn't done (SmartScreen and Gatekeeper still warn on the first install).

Signing happens only in the release workflow (`tauri.release.conf.json` turns on
`createUpdaterArtifacts`), so building locally needs no key. The workflow refuses to start if the
secret is missing.

### A release build knows its tag

The release workflow sets `FLAREITE_RELEASE_TAG` (and `FLAREITE_RELEASE_REPOSITORY`), which
`build.rs` checks against the manifests' version and the app compiles in. That tag is the running
version for updates, and the one Flareite shows. The manifests', which the installers are built
and signed as, stays what extensions are checked against. Builds without a tag (development and
local builds) don't update.

### Releases are picked by tag; the updater fetches and installs

Checking lists the repository's releases with the extension installer's GitHub client, and picks
the newest whose tag is past the running one, has a `latest.json`, and is in the channel:
`update.channel` is `stable`, `prerelease`, or `auto` (the default: prereleases if the running
build is one). The updater then reads that release's `latest.json` and downloads the installer for
this platform and kind of installation (`windows-x86_64-msi`, `linux-x86_64-deb`, ...), checking
its signature. Its version comparison is replaced: the release is newer by its tag, and its
manifest must name the tag's `major.minor.patch`.

Picking by tag lets prereleases be ordered at all, and lets the channel be the user's choice
without a manifest per channel. Its cost is that the signature binds only `major.minor.patch`:
someone able to publish a release could offer an older signed build of the same version (an
earlier alpha) as a newer one. They'd need the release rights, and could only offer a build that
was signed and released before.

### The workflow makes the manifest

`tauri-action` uploads each installer and its signature, but its own `latest.json` is merged by
each platform's job in turn, which races. Instead the publish job reads every `.sig` in the draft
and writes `latest.json` once (`scripts/updater-manifest.mjs`), refusing signatures made for
another version. Its entries are for every kind of installation the workflow builds, so an MSI
install updates with the MSI and a per-user NSIS install with the setup `.exe`, and `.deb` and
`.rpm` installs update with their packages (asking for an administrator's password).

### Checked at startup, at most once a day

Five seconds after startup, release builds check when `update.check` is on (the default) and the
last check was more than a day ago (`lastUpdateCheck` in `state.json`). This makes an exception to
ADR 0036's "no network unasked": one request to GitHub's API a day, out of the 60 an hour an
address may make, and the manifest from the release's storage. **Flareite: Check for Updates**
checks whenever asked. An update found is offered in a notification (Download, Release Notes) and
in the status bar.

### Downloaded, then installed on restart

The download stays in memory until **Restart to Update** (a notification, the status bar, or the
command). Restarting closes the window as closing it does, asking about unsaved changes, and the
update is installed last as Flareite exits, after terminals, extensions, and language servers have
stopped. Then Flareite starts again: on Windows the installer does (passive, with a progress bar);
elsewhere the updater replaces the app and Flareite starts the new one. An install that fails is
recorded in `state.json` and reported at the next start.

## Consequences

- Installs from before this release (`v0.1.0-alpha.1` and `alpha.2`) don't have the updater; they
  need the next release installed by hand once.
- The private key must be kept: installed copies trust only its public half, so losing it means
  publishing a release with a new key that every user installs by hand.
- A release whose workflow didn't finish has no `latest.json`, and isn't offered.
- An MSI installed for all users asks for elevation when it updates; declining leaves the old
  version, which doesn't start again on its own.
