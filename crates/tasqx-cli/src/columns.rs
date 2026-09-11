//! Fitting a row of columns to the width of the terminal.
//!
//! One algorithm for every table the CLI prints (#352). `render::TaskCols`
//! sizes `list` and `agenda` with it, and `projects`, `config list`,
//! `report`, `memory list`, `memory search` and `theme list` are laid out on
//! it. Before this only the task table read the
//! terminal's width. The other three hand-rolled their widths beside it and
//! wrapped, and a wrapped row destroys the alignment of every column at once.
//!
//! What a column asks for, where its floor is, and whether it may be dropped
//! are each table's own business. Everything after that is here, so that
//! every table gives way in the same order.

/// Cells between two columns.
pub(crate) const GAP: usize = 2;

/// One column as the fitter sees it: how wide it asks to be, how narrow it may
/// get, and whether it may go altogether.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Column {
    /// What the column asks for, which is its widest cell or its header,
    /// already capped at any ceiling. Zero means absent: it costs nothing, not
    /// even the gap before it.
    pub width: usize,
    /// Below this the column stops carrying information. A column already at or
    /// under its floor is never shrunk.
    pub floor: usize,
    /// Whether the fitter may drop the column when the row cannot fit with
    /// every column at its floor.
    pub droppable: bool,
    /// Among columns that are equally wide, the higher rank gives a cell first.
    /// Equal ranks go right. Every constructor sets 0, so a table that does not
    /// care gets the positional rule. `list` cares: a date cut to `due 2026-0…`
    /// says nothing, and a project name cut by the same cell still identifies
    /// itself.
    pub tie_rank: u8,
}

impl Column {
    /// A column that keeps its width: never shrunk, never dropped. For what a
    /// cut would falsify, such as a number.
    pub(crate) fn fixed(width: usize) -> Self {
        Column {
            width,
            floor: width,
            droppable: false,
            tie_rank: 0,
        }
    }

    /// A column that may shrink to `floor` but never goes.
    pub(crate) fn shrinks(width: usize, floor: usize) -> Self {
        Column {
            width,
            floor,
            droppable: false,
            tie_rank: 0,
        }
    }

    /// A column that may shrink to `floor`, and is dropped before the row
    /// overflows.
    pub(crate) fn drops(width: usize, floor: usize) -> Self {
        Column {
            width,
            floor,
            droppable: true,
            tie_rank: 0,
        }
    }

    /// This column, giving before equally wide columns of a lower rank.
    pub(crate) fn tie_rank(self, rank: u8) -> Self {
        Column {
            tie_rank: rank,
            ..self
        }
    }
}

/// The width of a row: every present column, plus one gap between neighbours.
pub(crate) fn total(widths: &[usize]) -> usize {
    let present = widths.iter().filter(|w| **w > 0).count();
    widths.iter().sum::<usize>() + GAP * present.saturating_sub(1)
}

/// Fit `cols` into `budget` cells and return each column's width, `0` for one
/// that was dropped.
///
/// Two passes, both taken from `render::TaskCols`, where they were found on a
/// real store:
///
/// - **Shrink from the widest.** Over budget, one cell at a time comes off
///   whichever column is currently widest above its floor. "Shrink the least
///   important column first" was tried in `list`, and it cut PROJECT and TAGS
///   to their floors while a 68-cell TASK column sat untouched. Taking from the
///   widest converges on columns of comparable size. Ties go to the higher
///   [`Column::tie_rank`], then to the rightmost column, so by default the
///   leftmost flexible column, where a table puts the thing the row is read
///   for, is cut last.
/// - **Then drop from the right.** Still over with every column at its floor,
///   droppable columns go from the right until the row fits. That order is
///   positional, so a reader can predict which column goes without reading
///   this function.
///
/// Once a column has been dropped, the shrink pass runs again from what the
/// survivors asked for, so the cells the drop freed go back to the columns
/// that gave them, by the same widest-first rule (#346). Keeping the floors
/// instead cut `memory list`'s titles to twelve cells beside ten empty ones.
///
/// A row that still does not fit after both passes overflows. The floors are
/// where a column stops meaning anything, and a table of columns that mean
/// nothing is not the better answer.
pub(crate) fn fit(cols: &[Column], budget: usize) -> Vec<usize> {
    let mut w: Vec<usize> = cols.iter().map(|c| c.width).collect();
    shrink(cols, &mut w, budget);
    let mut dropped = false;
    for i in (0..cols.len()).rev() {
        if total(&w) <= budget {
            break;
        }
        if cols[i].droppable && w[i] > 0 {
            w[i] = 0;
            dropped = true;
        }
    }
    if dropped {
        for (wi, c) in w.iter_mut().zip(cols) {
            if *wi > 0 {
                *wi = c.width;
            }
        }
        shrink(cols, &mut w, budget);
    }
    w
}

/// The shrink pass: over `budget`, one cell at a time off whichever present
/// column is widest above its floor.
fn shrink(cols: &[Column], w: &mut [usize], budget: usize) {
    let mut over = total(w).saturating_sub(budget);
    while over > 0 {
        // `max_by_key` keeps the LAST of equal maxima, so after the rank a
        // tie goes right.
        let Some(widest) = (0..cols.len())
            .filter(|&i| w[i] > cols[i].floor)
            .max_by_key(|&i| (w[i], cols[i].tie_rank))
        else {
            break;
        };
        w[widest] -= 1;
        over -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_that_fits_is_left_alone() {
        let cols = [Column::fixed(4), Column::drops(20, 8)];
        assert_eq!(fit(&cols, 80), vec![4, 20]);
    }

    #[test]
    fn an_absent_column_costs_no_gap() {
        assert_eq!(total(&[4, 0, 10]), 4 + GAP + 10);
        assert_eq!(total(&[0, 0]), 0);
    }

    /// The widest column gives first, and columns converge on comparable
    /// sizes rather than one being cut to its floor while another keeps room.
    #[test]
    fn the_widest_column_gives_first() {
        let cols = [Column::shrinks(30, 10), Column::drops(12, 8)];
        // 30 + 2 + 12 = 44 → 36: eight cells, all from the 30.
        assert_eq!(fit(&cols, 36), vec![22, 12]);
        // → 26: the 30 comes down to meet the 12, then they alternate.
        let w = fit(&cols, 26);
        assert_eq!(total(&w), 26);
        assert!(w[0].abs_diff(w[1]) <= 1, "{w:?}");
    }

    #[test]
    fn on_a_tie_the_leftmost_column_is_cut_last() {
        let cols = [Column::shrinks(20, 5), Column::shrinks(20, 5)];
        assert_eq!(fit(&cols, 41), vec![20, 19]);
    }

    #[test]
    fn a_higher_tie_rank_gives_first_whatever_its_position() {
        let cols = [
            Column::shrinks(20, 5),
            Column::shrinks(20, 5).tie_rank(1),
            Column::shrinks(20, 5),
        ];
        assert_eq!(fit(&cols, 63), vec![20, 19, 20]);
    }

    #[test]
    fn a_fixed_column_never_gives() {
        let cols = [Column::fixed(10), Column::shrinks(10, 4)];
        assert_eq!(fit(&cols, 16), vec![10, 4]);
    }

    /// At every floor and still over: droppable columns go from the right, and
    /// only as many as it takes.
    #[test]
    fn droppable_columns_go_from_the_right_until_it_fits() {
        let cols = [
            Column::shrinks(10, 10),
            Column::drops(8, 8),
            Column::fixed(6),
            Column::drops(8, 8),
        ];
        // 10+8+6+8 + 3 gaps = 38.
        assert_eq!(fit(&cols, 28), vec![10, 8, 6, 0]);
        assert_eq!(fit(&cols, 18), vec![10, 0, 6, 0]);
    }

    /// A dropped column's cells go back to the columns that gave them (#346).
    ///
    /// The fitter shrank every column to its floor before it dropped one, and
    /// kept the floors after the drop had made room. At 60 columns `memory
    /// list` cut every title to twelve cells (`release-p...`) beside ten
    /// empty ones: data cut where the terminal could hold it, which D120(c)
    /// rules out.
    #[test]
    fn cells_freed_by_a_drop_go_back_to_the_columns_that_gave_them() {
        let cols = [
            Column::shrinks(30, 10),
            Column::drops(40, 30),
            Column::fixed(20),
        ];
        // At every floor: 10 + 30 + 20 + 2 gaps = 64 > 50, so the middle goes,
        // and the first gets back what the row can now hold: 50 - 20 - 2.
        assert_eq!(fit(&cols, 50), vec![28, 0, 20]);
        // Never more than it asked for.
        assert_eq!(fit(&cols, 60), vec![30, 0, 20]);
    }

    /// Nothing left to give: the row overflows rather than cutting a column
    /// below the width where it stops meaning anything.
    #[test]
    fn a_row_with_nothing_left_to_give_overflows() {
        let cols = [Column::fixed(10), Column::shrinks(10, 8)];
        assert_eq!(fit(&cols, 5), vec![10, 8]);
    }
}
