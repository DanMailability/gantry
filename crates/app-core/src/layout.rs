//! Pixel-to-grid size calculations.

/// How many whole cells fit in `width`×`height` pixels, as `(cols, rows)`. Never smaller than 2×1.
pub fn grid_dims(width: f32, height: f32, cell_width: f32, cell_height: f32) -> (u16, u16) {
    let fit = |space: f32, cell: f32, min: u16| {
        let count = (space / cell).floor();
        if count.is_finite() && count >= min as f32 { count.min(u16::MAX as f32) as u16 } else { min }
    };
    (fit(width, cell_width, 2), fit(height, cell_height, 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divides_space_into_whole_cells() {
        assert_eq!(grid_dims(805.0, 370.0, 8.0, 18.0), (100, 20));
    }

    #[test]
    fn tiny_or_invalid_space_clamps_to_minimum() {
        assert_eq!(grid_dims(0.0, 0.0, 8.0, 18.0), (2, 1));
        assert_eq!(grid_dims(500.0, 500.0, 0.0, 0.0), (2, 1));
        assert_eq!(grid_dims(f32::NAN, 100.0, 8.0, 18.0), (2, 5));
    }
}
