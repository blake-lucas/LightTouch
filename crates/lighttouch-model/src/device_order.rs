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
}
