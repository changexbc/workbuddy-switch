//! Dedicated tray assets: macOS template alpha, color on Windows/Linux.
//! Keep dimensions checked at compile time; no runtime PNG decoder is needed.

#[cfg(target_os = "macos")]
pub fn tray_icon() -> tauri::image::Image<'static> {
    const RGBA: &[u8; 36 * 36 * 4] = include_bytes!("../icons/tray-icon-template.rgba");
    tauri::image::Image::new(RGBA, 36, 36)
}

#[cfg(not(target_os = "macos"))]
pub fn tray_icon() -> tauri::image::Image<'static> {
    const RGBA: &[u8; 32 * 32 * 4] = include_bytes!("../icons/tray-icon-color.rgba");
    tauri::image::Image::new(RGBA, 32, 32)
}

#[cfg(test)]
mod tests {
    #[test]
    fn tray_assets_keep_transparency_and_antialiased_edges() {
        // Validate both platforms even when the test runs only on macOS.
        for (pixels, size) in [
            (
                include_bytes!("../icons/tray-icon-template.rgba").as_slice(),
                36,
            ),
            (
                include_bytes!("../icons/tray-icon-color.rgba").as_slice(),
                32,
            ),
        ] {
            assert_eq!(pixels.len(), size * size * 4);
            let alpha: Vec<_> = pixels.chunks_exact(4).map(|pixel| pixel[3]).collect();
            assert!(alpha.contains(&0), "tray must not be an opaque square");
            assert!(
                alpha.iter().any(|&value| value >= 240),
                "visible silhouette"
            );
            assert!(
                alpha.iter().any(|&value| value > 0 && value < 240),
                "smooth edges"
            );
            for corner in [0, size - 1, size * (size - 1), size * size - 1] {
                assert_eq!(alpha[corner], 0);
            }
        }
        for pixel in include_bytes!("../icons/tray-icon-template.rgba").chunks_exact(4) {
            assert_eq!(&pixel[..3], &[0, 0, 0], "template is alpha-only black ink");
        }
    }
}
