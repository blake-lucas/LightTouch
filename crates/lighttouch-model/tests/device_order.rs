//! The device order as the library keeps it (the core's `.order` file): moves, new and deleted devices, and junk in
//! the file. Its own test binary: the core's library lock is a non-blocking flock, and a process another test forks
//! in parallel holds it for a moment (until its exec closes the descriptor), which made these flaky beside the
//! model's unit tests.
use lighttouch_core::library::Library;
use std::path::{Path, PathBuf};

/// A library of hand-made records (all the core's listing reads), in a scratch folder.
fn scratch(name: &str, ids: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lt-order-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for id in ids {
        add_record(&dir, id);
    }
    dir
}

fn add_record(dir: &Path, id: &str) {
    let record = dir.join("records").join(id);
    std::fs::create_dir_all(record.join("state")).unwrap();
    let json = format!(
        r#"{{"schema":1,"id":"{id}","name":"{id}","board":"n72ap","build":"7E18","base_sha256":"{}"}}"#,
        "0".repeat(64)
    );
    std::fs::write(record.join("record.json"), json).unwrap();
}

fn ids(library: &Library) -> Vec<String> {
    library.records().unwrap().into_iter().map(|r| r.id).collect()
}

#[test]
fn the_library_keeps_the_order_across_opens() {
    let dir = scratch("moves", &["a", "b", "c"]);
    let library = Library::open(&dir).unwrap();
    assert_eq!(ids(&library), ["a", "b", "c"]);
    library.move_to("c", 0).unwrap();
    library.move_to("a", 99).unwrap();
    assert_eq!(ids(&library), ["c", "b", "a"]);
    drop(library);
    let library = Library::open(&dir).unwrap();
    assert_eq!(ids(&library), ["c", "b", "a"]);
    assert!(library.move_to("zz", 0).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn new_devices_go_last_and_deleted_ones_drop_out() {
    let dir = scratch("changes", &["b", "c"]);
    let library = Library::open(&dir).unwrap();
    library.move_to("c", 0).unwrap();
    // A record the order doesn't name yet ("a" sorts first by id) is listed after the ordered ones.
    add_record(&dir, "a");
    assert_eq!(ids(&library), ["c", "b", "a"]);
    library.remove("b").unwrap();
    assert_eq!(ids(&library), ["c", "a"]);
    let order = std::fs::read_to_string(dir.join(".order")).unwrap();
    assert!(!order.contains('b'), "{order}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_ids_and_junk_in_the_order_are_ignored() {
    let dir = scratch("junk", &["a", "b", "c"]);
    std::fs::write(dir.join(".order"), "gone\nc\n../x\nc\n\n\u{1}\na\r\n").unwrap();
    let library = Library::open(&dir).unwrap();
    assert_eq!(ids(&library), ["c", "a", "b"]);
    std::fs::write(dir.join(".order"), [0xffu8, 0, 0xfe]).unwrap();
    assert_eq!(ids(&library), ["a", "b", "c"]);
    // A move writes a clean order again.
    library.move_to("b", 0).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join(".order")).unwrap(), "b\na\nc\n");
    let _ = std::fs::remove_dir_all(&dir);
}
