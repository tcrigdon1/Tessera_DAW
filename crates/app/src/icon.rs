//! The Tessera window icon, shared by the main and second windows.
//! `assets/tessera.ico` holds the same picture for desktop shortcuts.

use std::sync::{Arc, OnceLock};

use eframe::egui::IconData;

pub fn icon() -> Arc<IconData> {
    static ICON: OnceLock<Arc<IconData>> = OnceLock::new();
    ICON.get_or_init(|| {
        let png = include_bytes!("../assets/tessera.png");
        Arc::new(eframe::icon_data::from_png_bytes(png).unwrap_or_default())
    })
    .clone()
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_decodes() {
        let icon = super::icon();
        assert_eq!((icon.width, icon.height), (256, 256));
    }
}
