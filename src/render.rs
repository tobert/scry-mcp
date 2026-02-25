use crate::error::ScryError;
use std::sync::{Arc, LazyLock};
use usvg::fontdb;

/// Shared font database loaded once with system fonts.
static FONTDB: LazyLock<Arc<fontdb::Database>> = LazyLock::new(|| {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    tracing::info!("Loaded {} font faces", db.len());
    Arc::new(db)
});

pub fn svg_to_png(svg_str: &str) -> Result<Vec<u8>, ScryError> {
    let mut options = usvg::Options::default();
    options.fontdb = FONTDB.clone();
    let tree = usvg::Tree::from_str(svg_str, &options)?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| ScryError::Render("Failed to create pixmap (zero size?)".into()))?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map_err(|e| ScryError::Render(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_red_rect() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
            <rect fill="red" width="100" height="100"/>
        </svg>"#;
        let png = svg_to_png(svg).expect("render should succeed");
        // PNG magic bytes
        assert_eq!(&png[..4], &[137, 80, 78, 71]);
        assert!(png.len() > 100, "PNG should have meaningful content");
    }

    #[test]
    fn test_render_text() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="50">
            <text x="10" y="30" font-family="sans-serif" font-size="20" fill="black">Hello</text>
        </svg>"#;
        let png = svg_to_png(svg).expect("text render should succeed");
        assert_eq!(&png[..4], &[137, 80, 78, 71]);
    }

    #[test]
    fn test_render_invalid_svg() {
        let result = svg_to_png("not svg at all");
        assert!(result.is_err());
    }
}
