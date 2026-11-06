#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

/// Convert a logical caret origin to the client-area point used by IMM.
/// Keep the existing half-line-height offset and truncate each physical
/// coordinate before adding that offset.
pub(crate) fn physical_ime_position(
    origin: (f32, f32),
    line_height: f32,
    scale_factor: f32,
) -> (i32, i32) {
    (
        (origin.0 * scale_factor) as i32,
        (origin.1 * scale_factor) as i32 + ((line_height * scale_factor) as i32 / 2),
    )
}

#[cfg(test)]
mod tests {
    use super::physical_ime_position;

    #[test]
    fn scales_client_coordinates_and_line_height_for_dpi() {
        assert_eq!(physical_ime_position((10.0, 20.0), 16.0, 1.0), (10, 28));
        assert_eq!(physical_ime_position((10.0, 20.0), 16.0, 1.25), (12, 35));
        assert_eq!(physical_ime_position((10.0, 20.0), 16.0, 2.0), (20, 56));
    }

    #[test]
    fn keeps_negative_client_coordinates() {
        assert_eq!(physical_ime_position((-10.5, -20.5), 17.0, 1.0), (-10, -12));
        assert_eq!(physical_ime_position((-7.75, -11.75), 9.5, 1.25), (-9, -9));
    }

    #[test]
    fn preserves_separate_fractional_truncation() {
        assert_eq!(physical_ime_position((0.75, 0.75), 1.5, 1.0), (0, 0));
        assert_eq!(physical_ime_position((1.9, 2.9), 5.9, 1.25), (2, 6));
    }
}
