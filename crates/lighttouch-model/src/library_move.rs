//! Where the devices are kept (Settings → Storage): moving the device library to a folder the user chooses, or
//! switching to devices already kept in one. Logs and settings stay in the data folder; only the library goes,
//! with downloads waiting for a preparation (both are gigabytes).
use crate::Finished;
use crate::settings::Settings;
use crate::task::Reporter;
use lighttouch_core::app_log;
use lighttouch_core::files;
use lighttouch_core::paths::DataDirs;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What the library root holds that is the library's: anything else found there is the user's and stays.
const LIBRARY_ENTRIES: [&str; 4] = ["records", "selected", ".prepared", DOWNLOADS];
/// Downloads beside a library the user moved (`DataDirs::downloads`).
const DOWNLOADS: &str = ".downloads";
/// The core's library lock (Library.cpp) and each device's lease (`records/ID/state/.lease`), held while a
/// device runs.
const LIBRARY_LOCK: &str = ".library-lock";
const LEASE: &str = ".lease";
/// Kept free on the destination after a copy.
const SPARE_BYTES: u64 = 256 << 20;

/// What choosing a folder for the devices would do.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    /// `target` is empty or new: the current devices (`devices` of them, `bytes` on disk) move there, or stay
    /// where they are while LightTouch starts over there.
    New { target: PathBuf, devices: usize, bytes: u64 },
    /// `target` already holds `devices` LightTouch devices: LightTouch switches to them, and the current ones stay
    /// where they are.
    Existing { target: PathBuf, devices: usize },
}

impl Choice {
    pub fn target(&self) -> &Path {
        match self {
            Self::New { target, .. } | Self::Existing { target, .. } => target,
        }
    }
}

/// What choosing `picked` for the devices means. A folder that has other things in it gets a `LightTouch` folder
/// inside, so a later move never has to empty a folder the user keeps other files in.
pub fn plan(dirs: &DataDirs, picked: &Path) -> Result<Choice, String> {
    if !picked.is_absolute() {
        return Err(format!("{} isn't a full path", picked.display()));
    }
    let target = if is_library(picked) || is_empty(picked) {
        picked.to_path_buf()
    } else {
        let inside = picked.join("LightTouch");
        if !(is_library(&inside) || is_empty(&inside)) {
            return Err(format!("{} already has other files in it. Choose an empty folder.", inside.display()));
        }
        inside
    };
    let current = dirs.library();
    let (resolved, here) = (resolved(&target), resolved(&current));
    if resolved == here {
        return Err("Your devices are already kept there".into());
    }
    if resolved.starts_with(&here) || here.starts_with(&resolved) {
        return Err("Choose a folder outside the one your devices are in now".into());
    }
    // A library with no devices (left by "Don't move them") takes a move like an empty folder.
    if is_library(&target) && device_count(&target) > 0 {
        return Ok(Choice::Existing { devices: device_count(&target), target });
    }
    let mut bytes = LIBRARY_ENTRIES.iter().map(|name| files::stored_bytes(&current.join(name))).sum();
    if !dirs.downloads().starts_with(&current) {
        bytes += files::stored_bytes(&dirs.downloads());
    }
    Ok(Choice::New { target, devices: device_count(&current), bytes })
}

/// A LightTouch device library: `records/`, and nothing that isn't the library's (hidden names are its
/// temporaries).
pub fn is_library(dir: &Path) -> bool {
    dir.join("records").is_dir()
        && std::fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().all(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with('.') || LIBRARY_ENTRIES.contains(&name.as_str())
            })
        })
}

fn is_empty(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    }
}

fn device_count(library: &Path) -> usize {
    std::fs::read_dir(library.join("records"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("record.json").is_file())
        .count()
}

/// `path` with its existing part resolved (links, `..`), so two spellings of a folder compare equal.
fn resolved(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(real) = existing.canonicalize() {
            let real = lighttouch_core::paths::plain(&real);
            return rest.iter().rev().fold(real, |p: PathBuf, name: &&OsStr| p.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// A move of the library from `dirs` to `target`, to run on its own thread.
pub struct Move {
    pub from: PathBuf,
    pub from_downloads: PathBuf,
    /// The data folder's own library: the setting is cleared, not saved, when the devices go back there.
    pub default: PathBuf,
    pub to: PathBuf,
    /// The settings to save once the devices are in place, so LightTouch finds them there from then on.
    pub settings: Settings,
    pub settings_path: PathBuf,
    /// Tests turn renaming off to try the copy a move to another drive makes.
    rename: bool,
}

impl Move {
    pub fn new(dirs: &DataDirs, settings: &Settings, target: &Path) -> Self {
        Self {
            from: dirs.library(),
            from_downloads: dirs.downloads(),
            default: dirs.default_library(),
            to: target.to_path_buf(),
            settings: settings.clone(),
            settings_path: dirs.settings(),
            rename: true,
        }
    }

    fn to_downloads(&self) -> PathBuf {
        let root = self.settings_path.parent().map(Path::to_path_buf).unwrap_or_default();
        DataDirs::new(root).with_library(Some(self.to.clone())).downloads()
    }

    /// The library setting for `to`.
    fn setting(&self) -> Option<PathBuf> {
        (resolved(&self.to) != resolved(&self.default)).then(|| self.to.clone())
    }
}

/// One thing to move: renamed when it can be, else copied and the original removed once the copy is in use.
struct Item {
    from: PathBuf,
    to: PathBuf,
    renamed: bool,
    copied: bool,
}

/// Move the devices. Nothing may run meanwhile (the model checks its own session; another LightTouch's device
/// holds its lease, which this takes). On failure or cancel everything is put back as it was.
pub fn run(m: Move, reporter: &Reporter) -> Result<Finished, String> {
    let started = Instant::now();
    reporter.report("Checking your devices", 0, 0);
    // The library lock keeps another LightTouch from adding or removing devices meanwhile; the leases keep it from
    // starting one.
    let _library_lock = lock(&m.from.join(LIBRARY_LOCK), true)
        .map_err(|_| "Another LightTouch window is changing your devices. Try again when it's done.".to_string())?;
    let mut leases = leases(&m.from)?;

    let mut items: Vec<Item> = Vec::new();
    for name in LIBRARY_ENTRIES.iter().filter(|n| **n != DOWNLOADS) {
        let from = m.from.join(name);
        if from.exists() {
            items.push(Item { to: m.to.join(name), from, renamed: false, copied: false });
        }
    }
    let to_downloads = m.to_downloads();
    if resolved(&m.from_downloads) != resolved(&to_downloads) {
        for entry in std::fs::read_dir(&m.from_downloads).into_iter().flatten().flatten() {
            let to = to_downloads.join(entry.file_name());
            if !to.exists() {
                items.push(Item { from: entry.path(), to, renamed: false, copied: false });
            }
        }
    }
    // Never into anything already there: a failed move removes what it wrote, and that must be only its own. A
    // library left without devices has only empty folders, which make way.
    for item in &items {
        if std::fs::remove_dir(&item.to).is_err() && item.to.exists() {
            return Err(format!("{} already has LightTouch files in it. Choose an empty folder.", m.to.display()));
        }
    }
    let created_to = !m.to.exists();
    let result = move_items(&m, &mut items, &mut leases, &to_downloads, reporter);
    if let Err(e) = result {
        undo(&mut items, &m, created_to, &to_downloads);
        return Err(e);
    }

    // In place: from now on LightTouch looks for the devices there.
    let mut settings = m.settings.clone();
    settings.library = m.setting();
    if let Err(e) = settings.save(&m.settings_path) {
        undo(&mut items, &m, created_to, &to_downloads);
        return Err(format!("Couldn't save the new folder in the settings: {e}"));
    }

    // The originals of what was copied. Only the library's own entries go, then the folders if that left them empty:
    // anything else in them is the user's.
    drop(leases);
    drop(_library_lock);
    let copied: Vec<&Item> = items.iter().filter(|i| i.copied).collect();
    let total = copied.len() as u64;
    for (n, item) in copied.iter().enumerate() {
        reporter.report("Removing the old copy", n as u64, total);
        remove(&item.from);
    }
    let _ = std::fs::remove_dir(&m.from_downloads);
    let _ = std::fs::remove_file(m.from.join(LIBRARY_LOCK));
    let _ = std::fs::remove_dir(&m.from);

    let bytes = items.iter().map(|i| files::stored_bytes(&i.to)).sum::<u64>();
    let seconds = started.elapsed().as_secs_f64();
    let how = if items.iter().any(|i| i.copied) { "copied" } else { "renamed" };
    app_log!(
        "moved the devices from {} to {} ({how}, {:.1} GB) in {seconds:.1} s",
        m.from.display(),
        m.to.display(),
        bytes as f64 / 1e9
    );
    Ok(Finished::LibraryMoved { to: m.to.clone(), library: settings.library, bytes, seconds })
}

fn move_items(
    m: &Move,
    items: &mut [Item],
    leases: &mut Vec<File>,
    to_downloads: &Path,
    reporter: &Reporter,
) -> Result<(), String> {
    std::fs::create_dir_all(&m.to).map_err(|e| format!("Couldn't create {}: {e}", m.to.display()))?;
    if items.iter().any(|i| i.to.starts_with(to_downloads)) {
        std::fs::create_dir_all(to_downloads)
            .map_err(|e| format!("Couldn't create {}: {e}", to_downloads.display()))?;
    }
    // On the same drive a rename moves everything at once. Windows can't rename a folder with a file open in it, the
    // leases included; nothing can start a device in the moment between. A first rename that fails for any reason
    // turns the move into a copy, with the leases taken again.
    if m.rename && same_drive(&m.from, &m.to) {
        leases.clear();
        let mut any = false;
        for item in items.iter_mut() {
            match std::fs::rename(&item.from, &item.to) {
                Ok(()) => {
                    item.renamed = true;
                    any = true;
                }
                Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {}
                Err(e) if !any => {
                    app_log!("renaming {} failed ({e}); copying instead", item.from.display());
                    break;
                }
                Err(e) => return Err(format!("Couldn't move {}: {e}", item.from.display())),
            }
        }
        if items.iter().any(|i| !i.renamed) {
            *leases = self::leases(&m.from)?;
        }
    }
    let to_copy: Vec<usize> = (0..items.len()).filter(|&i| !items[i].renamed).collect();
    if to_copy.is_empty() {
        return Ok(());
    }
    let total: u64 = to_copy.iter().map(|&i| files::stored_bytes(&items[i].from)).sum();
    if let Some(free) = files::free_bytes(&m.to) {
        if free < total + SPARE_BYTES {
            return Err(format!(
                "There isn't enough free space in {}: your devices need {}, and {} is free",
                m.to.display(),
                size(total + SPARE_BYTES),
                size(free)
            ));
        }
    }
    let skip = |name: &OsStr| name == LEASE || name == LIBRARY_LOCK;
    let mut done = 0;
    for i in to_copy {
        let item = &mut items[i];
        item.copied = true; // partly, until it returns: removed by `undo` on failure
        files::copy_tree(&item.from, &item.to, &skip, &mut done, &mut |done| {
            reporter.report("Copying your devices", done.min(total), total)
        })
        .map_err(|e| {
            if e.kind() == io::ErrorKind::Interrupted {
                "Cancelled".to_string()
            } else {
                format!("Couldn't copy your devices: {e}")
            }
        })?;
    }
    reporter.report("Copying your devices", total, total);
    Ok(())
}

/// Put everything back after a failure: renames reversed, copies removed, created folders removed if empty.
fn undo(items: &mut [Item], m: &Move, created_to: bool, to_downloads: &Path) {
    for item in items.iter_mut().rev() {
        if item.renamed {
            if let Err(e) = std::fs::rename(&item.to, &item.from) {
                app_log!("couldn't move {} back to {}: {e}", item.to.display(), item.from.display());
            }
        } else if item.copied {
            remove(&item.to);
        }
    }
    let _ = std::fs::remove_dir(to_downloads);
    if created_to {
        let _ = std::fs::remove_dir(&m.to);
    }
}

fn remove(path: &Path) {
    let result = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    if let Err(e) = result {
        app_log!("couldn't remove {}: {e}", path.display());
    }
}

/// An exclusive lock on `path`, as the core takes it (flock; LockFileEx on Windows). `create` makes the file;
/// otherwise a missing one is `None` (a device never started has no lease).
fn lock(path: &Path, create: bool) -> io::Result<Option<File>> {
    let file = match std::fs::OpenOptions::new().read(true).write(true).create(create).truncate(false).open(path) {
        Ok(file) => file,
        Err(e) if !create && e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    file.try_lock().map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
    Ok(Some(file))
}

/// The leases of every device in the library `from`, or why not (one is running).
fn leases(from: &Path) -> Result<Vec<File>, String> {
    let mut leases = Vec::new();
    for record in std::fs::read_dir(from.join("records")).into_iter().flatten().flatten() {
        match lock(&record.path().join("state").join(LEASE), false) {
            Ok(Some(file)) => leases.push(file),
            Ok(None) => {}
            Err(_) => return Err("A device is running in another LightTouch window. Stop it first.".into()),
        }
    }
    Ok(leases)
}

/// Whether a rename can move `from` into `to` (or its nearest existing parent): the same drive.
#[cfg(unix)]
fn same_drive(from: &Path, to: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let device = |path: &Path| path.ancestors().find_map(|p| std::fs::metadata(p).ok()).map(|meta| meta.dev());
    device(from).is_some() && device(from) == device(to)
}

#[cfg(windows)]
fn same_drive(from: &Path, to: &Path) -> bool {
    let volume = |path: &Path| {
        let existing = path.ancestors().find(|p| p.exists())?;
        lighttouch_core::files::volume(existing).map(|v| v.to_lowercase())
    };
    volume(from).is_some() && volume(from) == volume(to)
}

#[cfg(not(any(unix, windows)))]
fn same_drive(_from: &Path, _to: &Path) -> bool {
    false
}

fn size(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Task;
    use std::sync::Arc;

    fn library(dir: &Path, ids: &[&str]) {
        for id in ids {
            std::fs::create_dir_all(dir.join("records").join(id).join("state")).unwrap();
            std::fs::write(dir.join("records").join(id).join("record.json"), b"{}").unwrap();
            std::fs::write(dir.join("records").join(id).join("state/.lease"), b"").unwrap();
        }
        std::fs::write(dir.join("selected"), ids[0]).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lt-move-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_with_other_files_gets_a_lighttouch_folder() {
        let root = scratch("plan");
        let dirs = DataDirs::new(root.join("data"));
        library(&dirs.library(), &["a", "b"]);
        let games = root.join("games");
        std::fs::create_dir_all(&games).unwrap();
        std::fs::write(games.join("notes.txt"), b"mine").unwrap();
        match plan(&dirs, &games).unwrap() {
            Choice::New { target, devices, .. } => {
                assert_eq!(target, games.join("LightTouch"));
                assert_eq!(devices, 2);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(plan(&dirs, &root.join("empty")).unwrap().target(), root.join("empty"));
        // A library already there is offered as it is.
        library(&root.join("usb"), &["c"]);
        assert_eq!(plan(&dirs, &root.join("usb")).unwrap(), Choice::Existing { target: root.join("usb"), devices: 1 });
        // Not into itself, not where it already is.
        assert!(plan(&dirs, &dirs.library().join("records/a/sub")).is_err());
        assert!(plan(&dirs, &dirs.library()).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn run_move(m: Move) -> Result<Finished, String> {
        let mut task = Task::spawn(Arc::new(|| {}), move |reporter| run(m, reporter));
        loop {
            if let Some(result) = task.take_result() {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn devices_move_and_come_back_with_their_downloads() {
        let root = scratch("run");
        let dirs = DataDirs::new(root.join("data"));
        library(&dirs.library(), &["a"]);
        std::fs::write(dirs.library().join("records/a/state/nand.qcow2"), b"blocks").unwrap();
        std::fs::create_dir_all(dirs.downloads()).unwrap();
        std::fs::write(dirs.downloads().join("x.ipsw"), b"zip").unwrap();
        let target = root.join("elsewhere/LightTouch");

        let Finished::LibraryMoved { library, .. } = run_move(Move::new(&dirs, &Settings::default(), &target)).unwrap()
        else {
            panic!()
        };
        assert_eq!(library.as_deref(), Some(target.as_path()));
        assert_eq!(Settings::load(&dirs.settings()).library.as_deref(), Some(target.as_path()));
        assert_eq!(std::fs::read(target.join("records/a/state/nand.qcow2")).unwrap(), b"blocks");
        assert_eq!(std::fs::read(target.join(".downloads/x.ipsw")).unwrap(), b"zip");
        assert!(!dirs.library().exists() && !dirs.downloads().exists());

        // And back to the data folder: the setting is cleared.
        let moved = dirs.clone().with_library(Some(target.clone()));
        let settings = Settings::load(&dirs.settings());
        let back = run_move(Move::new(&moved, &settings, &dirs.default_library())).unwrap();
        assert!(matches!(back, Finished::LibraryMoved { library: None, .. }));
        assert_eq!(Settings::load(&dirs.settings()).library, None);
        assert_eq!(std::fs::read(dirs.library().join("records/a/state/nand.qcow2")).unwrap(), b"blocks");
        assert_eq!(std::fs::read(dirs.downloads().join("x.ipsw")).unwrap(), b"zip");
        assert!(!target.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_move_to_another_drive_copies_then_removes_the_originals() {
        let root = scratch("copy");
        let dirs = DataDirs::new(root.join("data"));
        library(&dirs.library(), &["a", "b"]);
        std::fs::write(dirs.library().join("records/b/state/nand.qcow2"), vec![9u8; 3 << 20]).unwrap();
        std::fs::write(dirs.library().join("mine.txt"), b"the user's").unwrap();
        let target = root.join("usb");
        let mut m = Move::new(&dirs, &Settings::default(), &target);
        m.rename = false;
        let Finished::LibraryMoved { bytes, .. } = run_move(m).unwrap() else { panic!() };
        assert!(bytes >= 3 << 20);
        assert_eq!(std::fs::read(target.join("records/b/state/nand.qcow2")).unwrap(), vec![9u8; 3 << 20]);
        assert!(!target.join("records/a/state/.lease").exists());
        assert_eq!(std::fs::read(target.join("selected")).unwrap(), b"a");
        // The library's own entries are gone from the old folder; the user's file stays.
        assert!(!dirs.library().join("records").exists());
        assert_eq!(std::fs::read(dirs.library().join("mine.txt")).unwrap(), b"the user's");

        // Cancelled partway: nothing changes.
        let moved = dirs.clone().with_library(Some(target.clone()));
        let mut m = Move::new(&moved, &Settings::load(&dirs.settings()), &root.join("other"));
        m.rename = false;
        let mut task = Task::spawn(Arc::new(|| {}), move |reporter| {
            while !reporter.cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            run(m, reporter)
        });
        task.cancel();
        let result = loop {
            if let Some(result) = task.take_result() {
                break result;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(result.unwrap_err(), "Cancelled");
        assert!(!root.join("other").exists());
        assert!(target.join("records/b/state/nand.qcow2").is_file());
        assert_eq!(Settings::load(&dirs.settings()).library.as_deref(), Some(target.as_path()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A real library copied as to another drive: `LT_MOVE_DATA=<data folder> LT_MOVE_TO=<folder> cargo test -p
    /// lighttouch-model real_library_copy -- --ignored --nocapture`, then start a device from the data folder.
    #[test]
    #[ignore]
    fn real_library_copy() {
        let dirs = crate::data_dirs_in(PathBuf::from(std::env::var_os("LT_MOVE_DATA").unwrap()));
        let to = PathBuf::from(std::env::var_os("LT_MOVE_TO").unwrap());
        let mut m = Move::new(&dirs, &Settings::load(&dirs.settings()), &to);
        m.rename = false;
        let started = Instant::now();
        let result = run_move(m).unwrap();
        eprintln!("{result:?} in {:.1} s", started.elapsed().as_secs_f64());
    }

    #[test]
    fn a_move_never_writes_over_what_is_there() {
        let root = scratch("occupied");
        let dirs = DataDirs::new(root.join("data"));
        library(&dirs.library(), &["a"]);
        // Another library's devices: refused, and left alone.
        let other = root.join("other");
        library(&other, &["b"]);
        assert!(run_move(Move::new(&dirs, &Settings::default(), &other)).is_err());
        assert!(other.join("records/b/record.json").is_file() && dirs.library().join("records/a").is_dir());
        // A library with no devices (from "Don't move them") takes the move.
        let empty = root.join("empty");
        std::fs::create_dir_all(empty.join("records")).unwrap();
        assert!(matches!(plan(&dirs, &empty), Ok(Choice::New { .. })));
        run_move(Move::new(&dirs, &Settings::default(), &empty)).unwrap();
        assert!(empty.join("records/a/record.json").is_file());
        // A chosen folder that isn't there counts as missing until it has its library.
        let moved = dirs.clone().with_library(Some(root.join("unplugged")));
        assert_eq!(moved.missing_library(), Some(root.join("unplugged").as_path()));
        assert_eq!(dirs.clone().with_library(Some(empty.clone())).missing_library(), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_running_device_stops_the_move() {
        let root = scratch("busy");
        let dirs = DataDirs::new(root.join("data"));
        library(&dirs.library(), &["a"]);
        let held = lock(&dirs.library().join("records/a/state/.lease"), false).unwrap().unwrap();
        let target = root.join("elsewhere");
        let error = run_move(Move::new(&dirs, &Settings::default(), &target)).unwrap_err();
        assert!(error.contains("running"), "{error}");
        assert!(dirs.library().join("records/a/record.json").is_file() && !target.exists());
        drop(held);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
