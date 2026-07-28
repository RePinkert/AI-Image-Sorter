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
    if path.extension().and_then(|e| e.to_str()) == Some("webp") {
        // kamadak-exif supports webp via MediaCodec
        match exif_reader.read_from_container(&mut buf) {
            Ok(e) => exif_data = Some(e),
            Err(_) => {}
        }
    } else {
        match exif_reader.read_from_container(&mut buf) {
            Ok(e) => exif_data = Some(e),
            Err(_) => {}
        }
    }

    if let Some(exif) = exif_data {
        if let Some(field) = exif.get_field(exif::Tag::UserComment, exif::In::PRIMARY) {
            let comment = field.display_value().to_string();
            if comment.contains("Steps:") {
                return Ok(super::a1111_parse::parse_a1111_parameters(&comment));
            }
        }
    }
    Ok(ImageMeta {
        source_kind: SourceKind::Local,
        ..Default::default()
    })
}
