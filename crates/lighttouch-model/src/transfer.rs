//! Moving a device to another computer or phone: export it to a device file, import one (the core's Transfer.cpp,
//! the same format on every platform).
use crate::task::Reporter;
use crate::{Activity, Finished};
use lighttouch_core::app_log;
use lighttouch_core::library::Library;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The file name an export offers: the device's name (what a file system can't hold replaced), `.lighttouch.zip`.
pub fn file_name(device_name: &str) -> String {
    let safe: String =
        device_name.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '-' } else { c }).collect();
    let safe = safe.trim().trim_end_matches('.').trim();
    format!("{}.lighttouch.zip", if safe.is_empty() { "Device" } else { safe })
}

/// Who wrote an export, for its manifest.
pub fn app_name() -> String {
    format!("LightTouch {} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS)
}

/// The progress text for the core's phase.
pub fn phase_text(phase: &str) -> &'static str {
    match phase {
        "exporting" => "Exporting",
        "importing" => "Importing",
        "finishing" => "Finishing up",
        _ => "Working",
    }
}

fn message(e: lighttouch_core::Error) -> String {
    if e.is(lighttouch_core::sys::LT_CANCELLED) { "Cancelled".into() } else { e.to_string() }
}

/// Export `id` (named `name`) to `dest`, reporting through `reporter`.
pub fn export(library: &Path, id: &str, name: &str, dest: &Path, reporter: &Reporter) -> Result<Finished, String> {
    let started = Instant::now();
    let library = Library::open(library).map_err(|e| e.to_string())?;
    library
        .export(id, dest, &app_name(), &mut |phase, done, total| reporter.report(phase_text(phase), done, total))
        .map_err(message)?;
    let bytes = std::fs::metadata(dest).map_or(0, |m| m.len());
    let seconds = started.elapsed().as_secs_f64();
    app_log!("exported {name} to {} ({:.1} MB) in {seconds:.1} s", dest.display(), bytes as f64 / 1e6);
    Ok(Finished::DeviceExported { name: name.to_string(), path: dest.to_path_buf(), bytes, seconds })
}

/// Import the device in `source`; `select` makes it the selected device (nothing is running).
pub fn import(library: &Path, source: &Path, select: bool, reporter: &Reporter) -> Result<Finished, String> {
    let started = Instant::now();
    let library = Library::open(library).map_err(|e| e.to_string())?;
    let record = library
        .import_device(source, &mut |phase, done, total| reporter.report(phase_text(phase), done, total))
        .map_err(message)?;
    let selected = record.selected || (select && library.select(&record.id).is_ok());
    let seconds = started.elapsed().as_secs_f64();
    app_log!(
        "imported {} ({} {}) from {} in {seconds:.1} s",
        record.name,
        record.board,
        record.build,
        source.display()
    );
    Ok(Finished::DeviceImported { id: record.id, name: record.name, selected, seconds })
}

/// The activity's subject for the progress dialog: "Exporting iPod touch 4G 6.1.6".
pub fn title(activity: &Activity) -> Option<String> {
    match activity {
        Activity::Exporting { name, .. } => Some(format!("Exporting {name}")),
        Activity::Importing(path) => Some(format!(
            "Importing {}",
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "a device".into())
        )),
        Activity::MovingLibrary { .. } => Some("Moving your devices".into()),
        Activity::Preparing(_) => None,
    }
}

/// The export's destination, for messages.
pub fn destination(activity: &Activity) -> Option<&PathBuf> {
    match activity {
        Activity::Exporting { dest, .. } => Some(dest),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_safe_everywhere() {
        assert_eq!(file_name("iPod touch 4G 6.1.6"), "iPod touch 4G 6.1.6.lighttouch.zip");
        assert_eq!(file_name("a/b:c?"), "a-b-c-.lighttouch.zip");
        assert_eq!(file_name("  ..  "), "Device.lighttouch.zip");
    }
}
