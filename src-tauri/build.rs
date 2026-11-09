//! Tauri's build steps, and on Windows, the ConPTY that terminals use.
//!
//! The ConPTY built into Windows rewraps and repaints the screen when a
//! terminal is resized, which duplicates or loses lines (ADR 0011). The newer
//! one from Windows Terminal doesn't, and Microsoft publishes it for
//! applications to ship. It's downloaded once (a pinned version, checked
//! against its SHA-256) into `conpty/`. `tauri.windows.conf.json` lists it as
//! resources, so it's copied next to the executable in development and
//! installed there by the bundle, where `portable-pty` loads it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

use sha2::{Digest, Sha256};

/// NuGet package `Microsoft.Windows.Console.ConPTY` (MIT).
const CONPTY_VERSION: &str = "1.25.260930003";
const CONPTY_SHA256: &str = "02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee";

fn main() {
  if let Err(error) = check_release_tag() {
    println!("cargo::error={error}");
    return;
  }
  if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
    && let Err(error) = fetch_conpty()
  {
    println!("cargo::error=Couldn't get ConPTY {CONPTY_VERSION}: {error}");
    println!(
      "cargo::error=Building Flareite for Windows downloads it once from nuget.org into \
       src-tauri/conpty; check the network connection."
    );
    return;
  }
  if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
    make_room_for_resources();
  }
  tauri_build::build()
}

/// The release workflow names the tag it builds (`v0.2.0-alpha.1`) in
/// `FLAREITE_RELEASE_TAG`, which the app compiles in to know which releases
/// are newer (`app_update.rs`). Its `major.minor.patch` must be the
/// manifests' version, which the installers are built and signed as.
fn check_release_tag() -> Result<(), String> {
  println!("cargo::rerun-if-env-changed=FLAREITE_RELEASE_TAG");
  println!("cargo::rerun-if-env-changed=FLAREITE_RELEASE_REPOSITORY");
  let Ok(tag) = env::var("FLAREITE_RELEASE_TAG") else {
    return Ok(());
  };
  if tag.is_empty() {
    return Ok(());
  }
  let version = env::var("CARGO_PKG_VERSION").map_err(|error| error.to_string())?;
  let bare = tag.strip_prefix('v').unwrap_or(&tag);
  let base = bare.split(['-', '+']).next().unwrap_or_default();
  if base == version {
    Ok(())
  } else {
    Err(format!(
      "FLAREITE_RELEASE_TAG is {tag}, which isn't v{version} or a prerelease of it"
    ))
  }
}

/// tauri-build copies the resources next to the executable on every build,
/// even `cargo check`. A running Flareite holds them open (the loaded
/// `conpty.dll`, each terminal's `OpenConsole.exe`), and Windows won't
/// overwrite a file in use, but it will rename one. So copies in use are
/// moved aside, and a later build deletes them once they're free.
fn make_room_for_resources() {
  // tauri-build's target directory: OUT_DIR is <target dir>/build/<crate>/out.
  let Some(out_dir) = env::var_os("OUT_DIR") else {
    return;
  };
  let Some(target_dir) = Path::new(&out_dir).ancestors().nth(3) else {
    return;
  };
  if let Ok(entries) = fs::read_dir(target_dir) {
    for entry in entries.flatten() {
      if entry.file_name().to_string_lossy().ends_with(".in-use") {
        let _ = fs::remove_file(entry.path());
      }
    }
  }
  for name in ["conpty.dll", "OpenConsole.exe"] {
    let path = target_dir.join(name);
    if path.is_file() && in_use(&path) {
      let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
      if let Err(error) = fs::rename(&path, target_dir.join(format!("{name}.{stamp}.in-use"))) {
        println!(
          "cargo::warning=Couldn't move {} aside: {error}",
          path.display()
        );
      }
    }
  }
}

fn fetch_conpty() -> Result<(), String> {
  let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
    Ok("x86_64") => "x64",
    Ok("aarch64") => "arm64",
    Ok("x86") => "x86",
    other => return Err(format!("there is no build for {other:?}")),
  };
  let var = |name: &str| env::var(name).map_err(|error| format!("{name}: {error}"));
  let dir = PathBuf::from(var("CARGO_MANIFEST_DIR")?).join("conpty");
  println!("cargo::rerun-if-changed={}", dir.display());

  // Already there, for this version and architecture.
  let stamp = dir.join("version.txt");
  let wanted = format!("{CONPTY_VERSION} {arch}\n");
  let files = [dir.join("conpty.dll"), dir.join("OpenConsole.exe")];
  if fs::read_to_string(&stamp).ok().as_deref() == Some(wanted.as_str())
    && files.iter().all(|file| file.is_file())
  {
    return Ok(());
  }

  let out = PathBuf::from(var("OUT_DIR")?);
  let package = out.join("conpty.nupkg");
  let url = format!(
    "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/{CONPTY_VERSION}/\
     microsoft.windows.console.conpty.{CONPTY_VERSION}.nupkg"
  );
  run(
    Command::new(system_tool("curl.exe"))
      .args([
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--output",
      ])
      .arg(&package)
      .arg(&url),
  )?;
  let bytes = fs::read(&package).map_err(|error| format!("reading the download: {error}"))?;
  let digest: String = Sha256::digest(&bytes)
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect();
  if digest != CONPTY_SHA256 {
    return Err(format!(
      "the download's SHA-256 is {digest}, not {CONPTY_SHA256}"
    ));
  }

  // A .nupkg is a zip file; the tar in Windows reads zip files.
  let unpacked = out.join("conpty-package");
  let _ = fs::remove_dir_all(&unpacked);
  fs::create_dir_all(&unpacked).map_err(|error| format!("unpacking: {error}"))?;
  run(
    Command::new(system_tool("tar.exe"))
      .arg("-xf")
      .arg(&package)
      .arg("-C")
      .arg(&unpacked),
  )?;

  fs::create_dir_all(&dir).map_err(|error| format!("creating {}: {error}", dir.display()))?;
  let sources = [
    unpacked.join(format!("runtimes/win-{arch}/native/conpty.dll")),
    unpacked.join(format!("build/native/runtimes/{arch}/OpenConsole.exe")),
  ];
  for (source, target) in sources.iter().zip(&files) {
    fs::copy(source, target).map_err(|error| format!("copying {}: {error}", source.display()))?;
  }
  fs::write(&stamp, wanted).map_err(|error| format!("writing {}: {error}", stamp.display()))
}

/// A tool from `System32` (curl and tar ship with Windows 10 1803 and later),
/// so a different `tar` on the `PATH` (Git's can't read zip files) isn't used.
/// Whether `path` can't be written, as when it's a running program or a
/// loaded library (their mappings aren't open handles, so only a write
/// shows it). Opening for writing changes nothing.
fn in_use(path: &Path) -> bool {
  fs::OpenOptions::new().write(true).open(path).is_err()
}

fn system_tool(name: &str) -> PathBuf {
  env::var_os("SystemRoot")
    .map(|root| Path::new(&root).join("System32").join(name))
    .filter(|path| path.is_file())
    .unwrap_or_else(|| PathBuf::from(name))
}

fn run(command: &mut Command) -> Result<(), String> {
  let program = command.get_program().to_string_lossy().into_owned();
  let output = command
    .output()
    .map_err(|error| format!("running {program}: {error}"))?;
  if output.status.success() {
    Ok(())
  } else {
    Err(format!(
      "{program} failed ({}): {}",
      output.status,
      String::from_utf8_lossy(&output.stderr).trim()
    ))
  }
}
