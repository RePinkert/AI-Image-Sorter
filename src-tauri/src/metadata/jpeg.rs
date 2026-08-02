use anyhow::Result;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use super::{ImageMeta, SourceKind};

pub fn parse_jpeg_webp(path: &Path) -> Result<ImageMeta> {
    let file = File::open(path)?;
    let mut buf = BufReader::new(file);
    let exif_reader = exif::Reader::new();
    let mut exif_data = None;
    // Try to find EXIF segment.
    match exif_reader.read_from_container(&mut buf) {
        Ok(e) => exif_data = Some(e),
        Err(_) => {}
    }

    let mut meta = if let Some(exif) = exif_data {
        if let Some(field) = exif.get_field(exif::Tag::UserComment, exif::In::PRIMARY) {
            let comment = field.display_value().to_string();
            if comment.contains("Steps:") {
                super::a1111_parse::parse_a1111_parameters(&comment)
            } else {
                ImageMeta {
                    source_kind: SourceKind::Local,
                    ..Default::default()
                }
            }
        } else {
            ImageMeta {
                source_kind: SourceKind::Local,
                ..Default::default()
            }
        }
    } else {
        ImageMeta {
            source_kind: SourceKind::Local,
            ..Default::default()
        }
    };

    // Dimensions: header-only read (no pixel decode).
    if let Some(reader) = image::ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
    {
        if let Ok(dims) = reader.into_dimensions() {
            meta.width = dims.0;
            meta.height = dims.1;
        }
    }
    meta.refresh_generation_recipe();
    Ok(meta)
}
