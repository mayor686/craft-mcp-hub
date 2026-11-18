//! Finding an app's command-line tool on this computer.
//!
//! Windows is verified, and so is macOS: there the ArtCraft launcher installs only the desktop app
//! (`/Applications/<Name>.app`), whose bundle has no command-line tool. The tool is a separate
//! download, which the hub fetches itself into its tools folder (see `fetch`). The Linux locations
//! follow the catalog (Flatpak id, package name) and the usual conventions.

use crate::catalog::CatalogApp;
use crate::paths;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn unique(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !item.is_empty() && !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// The names the command-line tool may have: `<binary>-cli`, also under the app's former ids.
fn cli_names(app: &CatalogApp) -> Vec<String> {
    let binary = if app.binary.is_empty() { app.id.clone() } else { app.binary.clone() };
    unique(std::iter::once(binary).chain(app.former_ids.iter().cloned())).into_iter().map(|b| format!("{b}-cli")).collect()
}

/// The names the app's install folder may have.
fn folder_names(app: &CatalogApp) -> Vec<String> {
    unique(
        [app.name.clone(), app.mac_bundle_name.clone(), app.id.clone(), app.binary.clone()]
            .into_iter()
            .chain(app.former_names.iter().cloned())
            .chain(app.former_ids.iter().cloned()),
    )
}

#[cfg(windows)]
fn install_dirs(app: &CatalogApp) -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
    bases.push(paths::local_data_dir().join("Programs"));
    bases.push(paths::local_data_dir());
    bases.dedup();
    let names = folder_names(app);
    let mut dirs = Vec::new();
    for base in &bases {
        for suite in ["ArtCraft", "Storytold"] {
            let suite_dir = base.join(suite);
            for name in &names {
                dirs.push(suite_dir.join(name));
                dirs.push(suite_dir.join(name).join("bin"));
            }
        }
        for name in &names {
            dirs.push(base.join(name));
            dirs.push(base.join(name).join("bin"));
        }
    }
    dirs
}

#[cfg(target_os = "macos")]
fn install_dirs(app: &CatalogApp) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for base in [PathBuf::from("/Applications"), paths::home_dir().join("Applications")] {
        for name in folder_names(app) {
            let contents = base.join(format!("{name}.app")).join("Contents");
            for sub in ["MacOS", "Resources", "Resources/bin", "Helpers"] {
                dirs.push(contents.join(sub));
            }
        }
    }
    dirs.extend([PathBuf::from("/usr/local/bin"), PathBuf::from("/opt/homebrew/bin"), paths::home_dir().join(".local/bin")]);
    dirs
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_dirs(app: &CatalogApp) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin"), paths::home_dir().join(".local/bin")];
    let binary = if app.binary.is_empty() { app.id.clone() } else { app.binary.clone() };
    for name in unique(std::iter::once(binary).chain(folder_names(app))) {
        dirs.push(PathBuf::from("/opt").join(&name));
        dirs.push(PathBuf::from("/opt").join(&name).join("bin"));
        dirs.push(PathBuf::from("/usr/lib").join(&name));
    }
    dirs
}

/// A Flatpak install: the tool runs inside the app's sandbox.
#[cfg(all(unix, not(target_os = "macos")))]
fn flatpak(app: &CatalogApp) -> Option<Vec<String>> {
    if app.flatpak_id.is_empty() {
        return None;
    }
    let installed = [PathBuf::from("/var/lib/flatpak/app"), paths::local_data_dir().join("flatpak/app")].iter().any(|base| base.join(&app.flatpak_id).is_dir());
    let flatpak = paths::find_in_path("flatpak")?;
    let cli = cli_names(app).into_iter().next()?;
    installed.then(|| vec![flatpak.to_string_lossy().into_owned(), "run".to_string(), format!("--command={cli}"), app.flatpak_id.clone()])
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn flatpak(_app: &CatalogApp) -> Option<Vec<String>> {
    None
}

/// How to run the app's command-line tool, or `None` when the app isn't installed.
///
/// The answer is kept for a few seconds. Looking through every install folder and PATH for every
/// app adds up when the panel refreshes every couple of seconds, and apps aren't installed that often.
type Found = HashMap<String, (Instant, Option<Vec<String>>)>;
static FOUND: Mutex<Option<Found>> = Mutex::new(None);

pub fn find_cli(app: &CatalogApp) -> Option<Vec<String>> {
    const KEEP: Duration = Duration::from_secs(15);

    let mut found = FOUND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let found = found.get_or_insert_with(HashMap::new);
    if let Some((at, cli)) = found.get(&app.id) {
        // A tool that was found must still be there; one that wasn't is looked for again later.
        let still_valid = cli.as_ref().is_none_or(|cli| cli.first().is_some_and(|program| Path::new(program).is_file()));
        if at.elapsed() < KEEP && still_valid {
            return cli.clone();
        }
    }
    let cli = search(app);
    found.insert(app.id.clone(), (Instant::now(), cli.clone()));
    cli
}

/// The folder the hub downloads the app's command-line tool into.
pub fn tool_dir(app: &CatalogApp) -> PathBuf {
    paths::tools_dir().join(&app.id)
}

/// Forgets what was found for the app, so the next look sees a tool that was just installed.
pub fn forget(id: &str) {
    if let Ok(mut found) = FOUND.lock() {
        if let Some(found) = found.as_mut() {
            found.remove(id);
        }
    }
}

/// The desktop app's bundle (`<Name>.app`) in /Applications or ~/Applications.
#[cfg(target_os = "macos")]
pub fn mac_bundle(app: &CatalogApp) -> Option<PathBuf> {
    let names = folder_names(app);
    [PathBuf::from("/Applications"), paths::home_dir().join("Applications")]
        .iter()
        .flat_map(|base| names.iter().map(move |name| base.join(format!("{name}.app"))))
        .find(|bundle| bundle.join("Contents/Info.plist").is_file())
}

#[cfg(not(target_os = "macos"))]
pub fn mac_bundle(_app: &CatalogApp) -> Option<PathBuf> {
    None
}

/// Whether the desktop app is installed but its command-line tool isn't, and the hub can fetch it.
pub fn cli_fetchable(app: &CatalogApp) -> bool {
    !app.repo.is_empty() && mac_bundle(app).is_some()
}

fn search(app: &CatalogApp) -> Option<Vec<String>> {
    let names = cli_names(app);
    let fetched = tool_dir(app);
    for dir in install_dirs(app).into_iter().chain(std::iter::once(fetched)) {
        for name in &names {
            let file = dir.join(paths::exe_name(name));
            if file.is_file() {
                return Some(vec![file.to_string_lossy().into_owned()]);
            }
        }
    }
    for name in &names {
        if let Some(file) = paths::find_in_path(name) {
            return Some(vec![file.to_string_lossy().into_owned()]);
        }
    }
    flatpak(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_follow_the_catalog() {
        let app = CatalogApp { id: "pdfcraft".into(), binary: "pdfcraft".into(), former_ids: vec!["printcraft".into()], ..Default::default() };
        assert_eq!(cli_names(&app), ["pdfcraft-cli", "printcraft-cli"]);
        let nameless = CatalogApp { id: "newcraft".into(), ..Default::default() };
        assert_eq!(cli_names(&nameless), ["newcraft-cli"]);
    }
}
