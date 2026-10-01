//! Pixel-to-grid size calculations.

/// Smallest grid a pane gets. Reflowing to fewer columns multiplies the lines of scrollback until the oldest fall
/// off, so a narrower pane clips its grid instead.
pub const MIN_COLS: u16 = 20;
/// Smallest number of rows a pane gets.
pub const MIN_ROWS: u16 = 5;

/// How many whole cells fit in `width`×`height` pixels, as `(cols, rows)`. Never smaller than `MIN_COLS`×`MIN_ROWS`.
pub fn grid_dims(width: f32, height: f32, cell_width: f32, cell_height: f32) -> (u16, u16) {
    let fit = |space: f32, cell: f32, min: u16| {
        let count = (space / cell).floor();
        if count.is_finite() && count >= min as f32 { count.min(u16::MAX as f32) as u16 } else { min }
    };
    (fit(width, cell_width, MIN_COLS), fit(height, cell_height, MIN_ROWS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divides_space_into_whole_cells() {
        assert_eq!(grid_dims(805.0, 370.0, 8.0, 18.0), (100, 20));
    }

    #[test]
    fn tiny_or_invalid_space_keeps_the_minimum_grid() {
        assert_eq!(grid_dims(0.0, 0.0, 8.0, 18.0), (MIN_COLS, MIN_ROWS));
        assert_eq!(grid_dims(80.0, 36.0, 8.0, 18.0), (20, 5));
        assert_eq!(grid_dims(500.0, 500.0, 0.0, 0.0), (MIN_COLS, MIN_ROWS));
        assert_eq!(grid_dims(f32::NAN, 180.0, 8.0, 18.0), (MIN_COLS, 10));
    }
}
