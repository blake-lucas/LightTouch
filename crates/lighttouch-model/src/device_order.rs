//! Reordering the device list by dragging: where a dragged row lands. The order itself is the library's (the core's
//! `order` file, `Library::move_to`), so both apps share it; new devices go last and deleted ones drop out there.

/// The gap a row dropped at `y` goes into: 0 is above the first row, `centers.len()` below the last. `centers` are
/// the rows' vertical centres, top to bottom, as listed before the move.
pub fn gap_at(centers: &[f32], y: f32) -> usize {
    centers.iter().take_while(|c| **c < y).count()
}

/// Where the row at `from` ends up when dropped into `gap` (as `gap_at` counts them, before the move), or `None`
/// when that leaves it where it is (the gaps just above and just below it).
pub fn destination(from: usize, gap: usize, len: usize) -> Option<usize> {
    if from >= len || gap > len || gap == from || gap == from + 1 {
        return None;
    }
    Some(if gap > from { gap - 1 } else { gap })
}

/// Move the item at `from` to `to` (both in range), the rest keeping their order.
pub fn move_item<T>(list: &mut Vec<T>, from: usize, to: usize) {
    let item = list.remove(from);
    list.insert(to.min(list.len()), item);
}

#[cfg(test)]
mod tests {
    use super::*;
    use lighttouch_core::library::Library;
    use std::path::{Path, PathBuf};

    fn moved(list: &[&str], from: usize, gap: usize) -> Vec<String> {
        let mut list: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        if let Some(to) = destination(from, gap, list.len()) {
            move_item(&mut list, from, to);
        }
        list
    }

    #[test]
    fn a_drop_lands_in_the_gap_under_the_pointer() {
        let centers = [29.0, 89.0, 149.0];
        assert_eq!(gap_at(&centers, 0.0), 0);
        assert_eq!(gap_at(&centers, 30.0), 1);
        assert_eq!(gap_at(&centers, 148.0), 2);
        assert_eq!(gap_at(&centers, 500.0), 3);
        assert_eq!(gap_at(&[], 10.0), 0);
    }

    #[test]
    fn moves_up_and_down_count_the_gap_before_the_move() {
        let list = ["a", "b", "c", "d"];
        // Down: dropped below c (gap 3) is index 2 once a is out of the way.
        assert_eq!(moved(&list, 0, 3), ["b", "c", "a", "d"]);
        assert_eq!(moved(&list, 0, 4), ["b", "c", "d", "a"]);
        // Up.
        assert_eq!(moved(&list, 3, 0), ["d", "a", "b", "c"]);
        assert_eq!(moved(&list, 2, 1), ["a", "c", "b", "d"]);
        // Just above or below itself, or out of range: no move.
        assert_eq!(destination(1, 1, 4), None);
        assert_eq!(destination(1, 2, 4), None);
        assert_eq!(destination(1, 5, 4), None);
        assert_eq!(destination(4, 0, 4), None);
    }

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
        let order = std::fs::read_to_string(dir.join("order")).unwrap();
        assert!(!order.contains('b'), "{order}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_ids_and_junk_in_the_order_are_ignored() {
        let dir = scratch("junk", &["a", "b", "c"]);
        std::fs::write(dir.join("order"), "gone\nc\n../x\nc\n\n\u{1}\na\r\n").unwrap();
        let library = Library::open(&dir).unwrap();
        assert_eq!(ids(&library), ["c", "a", "b"]);
        std::fs::write(dir.join("order"), [0xffu8, 0, 0xfe]).unwrap();
        assert_eq!(ids(&library), ["a", "b", "c"]);
        // A move writes a clean order again.
        library.move_to("b", 0).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("order")).unwrap(), "b\na\nc\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
