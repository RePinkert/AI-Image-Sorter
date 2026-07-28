use anyhow::Result;
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgba};
use std::path::{Path, PathBuf};

/// Generate a 2x2 collage thumbnail for a group, picking up to 4 images.
/// Returns the path to the cached JPEG file.
pub fn get_or_create_group_thumbnail(
    group_key: &str,
    image_paths: &[PathBuf],
    cache_dir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(cache_dir)?;
    let thumb_path = cache_dir.join(format!("group_{}.jpg", group_key));
    if thumb_path.exists() {
        return Ok(thumb_path);
    }

    let cell = 160u32;
    let pad = 2u32;
    let total = cell * 2 + pad * 3;
    let mut canvas: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(total, total, Rgba([20, 20, 26, 255]));

    let picks: Vec<&PathBuf> = image_paths.iter().take(4).collect();
    for (i, img_path) in picks.iter().enumerate() {
        let col = i as u32 % 2;
        let row = i as u32 / 2;
        let x = pad + col * (cell + pad);
        let y = pad + row * (cell + pad);
        if let Ok(img) = image::open(img_path) {
            let thumb = resize_cover(&img, cell, cell);
            overlay(&mut canvas, &thumb, x, y);
        }
    }

    // If fewer than 4, fill remaining cells with dark placeholder (already default bg).
    image::DynamicImage::ImageRgba8(canvas)
        .into_rgb8()
        .save(&thumb_path)?;
    Ok(thumb_path)
}

fn resize_cover(img: &DynamicImage, target_w: u32, target_h: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return DynamicImage::new_rgba8(target_w, target_h);
    }
    let scale_w = target_w as f32 / w as f32;
    let scale_h = target_h as f32 / h as f32;
    let scale = scale_w.max(scale_h);
    let new_w = (w as f32 * scale).round() as u32;
    let new_h = (h as f32 * scale).round() as u32;
    let resized = img.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3);
    // crop center
    let crop_x = (new_w - target_w) / 2;
    let crop_y = (new_h - target_h) / 2;
    resized.crop_imm(crop_x, crop_y, target_w, target_h)
}

fn overlay(canvas: &mut ImageBuffer<Rgba<u8>, Vec<u8>>, img: &DynamicImage, x: u32, y: u32) {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    for dy in 0..h {
        for dx in 0..w {
            let px = rgba.get_pixel(dx, dy);
            canvas.put_pixel(x + dx, y + dy, *px);
        }
    }
}
