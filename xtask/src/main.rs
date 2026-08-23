#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::error::Error;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use ico::{IconDir, IconDirEntry, IconImage, ResourceType};
use image::imageops::FilterType;
use image::{ImageFormat, Rgba, RgbaImage};

const ICON_SIZES: [u32; 8] = [16, 20, 24, 32, 48, 64, 128, 256];
const WIZARD_IMAGE_SIZE: (u32, u32) = (240, 459);

fn main() -> Result<(), Box<dyn Error>> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("xtask has no workspace parent")?
        .to_owned();
    let asset_dir = workspace.join("assets/tray");
    let source_path = asset_dir.join("winsched-tray-generated.png");
    let source = image::open(&source_path)?.into_rgba8();
    let icon = trim_transparent_padding(&extract_edge_background(source));
    validate_alpha(&icon)?;

    let normalized_path = asset_dir.join("winsched-tray.png");
    icon.save_with_format(&normalized_path, ImageFormat::Png)?;

    let mut directory = IconDir::new(ResourceType::Icon);
    for size in ICON_SIZES {
        let resized = resize_premultiplied(&icon, size);
        let png_path = asset_dir.join(format!("winsched-tray-{size}.png"));
        resized.save_with_format(&png_path, ImageFormat::Png)?;
        let image = IconImage::from_rgba_data(size, size, resized.into_raw());
        directory.add_entry(IconDirEntry::encode(&image)?);
    }

    let ico_path = asset_dir.join("winsched.ico");
    directory.write(File::create(&ico_path)?)?;
    validate_ico(&ico_path)?;
    generate_installer_images(&workspace, &icon)?;
    println!(
        "generated {} and {} PNG sizes from {}",
        ico_path.display(),
        ICON_SIZES.len(),
        source_path.display()
    );
    Ok(())
}

fn generate_installer_images(workspace: &Path, icon: &RgbaImage) -> Result<(), Box<dyn Error>> {
    let asset_dir = workspace.join("assets/installer");
    fs::create_dir_all(&asset_dir)?;

    let foreground = resize_premultiplied(icon, 196);
    let dark_foreground = dark_theme_icon(&foreground);
    let light = wizard_panel(
        &foreground,
        Rgba([239, 246, 255, 255]),
        Rgba([207, 227, 252, 255]),
    );
    let dark = wizard_panel(
        &dark_foreground,
        Rgba([30, 42, 62, 255]),
        Rgba([17, 25, 39, 255]),
    );
    light.save_with_format(asset_dir.join("winsched-wizard.png"), ImageFormat::Png)?;
    dark.save_with_format(asset_dir.join("winsched-wizard-dark.png"), ImageFormat::Png)?;
    resize_premultiplied(icon, 128).save_with_format(
        asset_dir.join("winsched-wizard-small.png"),
        ImageFormat::Png,
    )?;
    dark_theme_icon(&resize_premultiplied(icon, 128)).save_with_format(
        asset_dir.join("winsched-wizard-small-dark.png"),
        ImageFormat::Png,
    )?;
    Ok(())
}

fn wizard_panel(icon: &RgbaImage, top: Rgba<u8>, bottom: Rgba<u8>) -> RgbaImage {
    let (width, height) = WIZARD_IMAGE_SIZE;
    let mut panel = RgbaImage::new(width, height);
    for y in 0..height {
        let denominator = height.saturating_sub(1).max(1);
        for x in 0..width {
            let mut pixel = [0; 4];
            for (index, channel) in pixel.iter_mut().enumerate() {
                let start = u32::from(top[index]);
                let end = u32::from(bottom[index]);
                *channel = u8::try_from(
                    (start * (denominator - y) + end * y + denominator / 2) / denominator,
                )
                .expect("interpolated color channel fits u8");
            }
            panel.put_pixel(x, y, Rgba(pixel));
        }
    }
    let x = i64::from((width - icon.width()) / 2);
    let y = i64::from((height - icon.height()) / 2);
    image::imageops::overlay(&mut panel, icon, x, y);
    panel
}

fn dark_theme_icon(icon: &RgbaImage) -> RgbaImage {
    let mut result = icon.clone();
    for pixel in result.pixels_mut() {
        let darkest = pixel[0].max(pixel[1]).max(pixel[2]);
        if pixel[3] != 0 && darkest < 100 {
            pixel.0[..3].copy_from_slice(&[213, 226, 244]);
        }
    }
    result
}

fn extract_edge_background(mut image: RgbaImage) -> RgbaImage {
    if image.pixels().any(|pixel| pixel[3] != u8::MAX) {
        return image;
    }

    let (width, height) = image.dimensions();
    let mut queue = VecDeque::new();
    let mut visited = vec![false; (width as usize) * (height as usize)];

    for x in 0..width {
        enqueue_if_background(&image, &mut visited, &mut queue, x, 0);
        enqueue_if_background(&image, &mut visited, &mut queue, x, height - 1);
    }
    for y in 0..height {
        enqueue_if_background(&image, &mut visited, &mut queue, 0, y);
        enqueue_if_background(&image, &mut visited, &mut queue, width - 1, y);
    }

    while let Some((x, y)) = queue.pop_front() {
        image.put_pixel(x, y, Rgba([0, 0, 0, 0]));
        for (next_x, next_y) in neighbors(x, y, width, height) {
            enqueue_if_background(&image, &mut visited, &mut queue, next_x, next_y);
        }
    }
    image
}

fn trim_transparent_padding(image: &RgbaImage) -> RgbaImage {
    let Some((minimum_x, minimum_y, maximum_x, maximum_y)) = alpha_bounds(image) else {
        return image.clone();
    };
    let content_width = maximum_x - minimum_x + 1;
    let content_height = maximum_y - minimum_y + 1;
    let padding = content_width.max(content_height).div_ceil(32);
    let left = minimum_x.saturating_sub(padding);
    let top = minimum_y.saturating_sub(padding);
    let right = (maximum_x + padding).min(image.width() - 1);
    let bottom = (maximum_y + padding).min(image.height() - 1);
    image::imageops::crop_imm(image, left, top, right - left + 1, bottom - top + 1).to_image()
}

fn alpha_bounds(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] <= 8 {
            continue;
        }
        bounds = Some(match bounds {
            None => (x, y, x, y),
            Some((minimum_x, minimum_y, maximum_x, maximum_y)) => (
                minimum_x.min(x),
                minimum_y.min(y),
                maximum_x.max(x),
                maximum_y.max(y),
            ),
        });
    }
    bounds
}

fn enqueue_if_background(
    image: &RgbaImage,
    visited: &mut [bool],
    queue: &mut VecDeque<(u32, u32)>,
    x: u32,
    y: u32,
) {
    let index = (y as usize) * (image.width() as usize) + (x as usize);
    if !visited[index] && is_checkerboard_pixel(*image.get_pixel(x, y)) {
        visited[index] = true;
        queue.push_back((x, y));
    }
}

fn is_checkerboard_pixel(pixel: Rgba<u8>) -> bool {
    let [red, green, blue, alpha] = pixel.0;
    let minimum = red.min(green).min(blue);
    let maximum = red.max(green).max(blue);
    alpha == u8::MAX && minimum >= 220 && maximum - minimum <= 10
}

fn neighbors(x: u32, y: u32, width: u32, height: u32) -> impl Iterator<Item = (u32, u32)> {
    [
        x.checked_sub(1).map(|next| (next, y)),
        (x + 1 < width).then_some((x + 1, y)),
        y.checked_sub(1).map(|next| (x, next)),
        (y + 1 < height).then_some((x, y + 1)),
    ]
    .into_iter()
    .flatten()
}

fn resize_premultiplied(image: &RgbaImage, size: u32) -> RgbaImage {
    let mut premultiplied = image.clone();
    for pixel in premultiplied.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = u8::try_from((u16::from(*channel) * alpha + 127) / 255)
                .expect("premultiplied color fits u8");
        }
    }

    let mut resized = image::imageops::resize(&premultiplied, size, size, FilterType::Lanczos3);
    for pixel in resized.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            pixel.0[..3].fill(0);
        } else {
            for channel in &mut pixel.0[..3] {
                let value = (u16::from(*channel) * 255)
                    .checked_div(alpha)
                    .expect("nonzero alpha checked above")
                    .min(255);
                *channel = u8::try_from(value).expect("unpremultiplied color fits u8");
            }
        }
    }
    resized
}

fn validate_alpha(image: &RgbaImage) -> Result<(), Box<dyn Error>> {
    let pixels = u64::from(image.width()) * u64::from(image.height());
    let transparent = image.pixels().filter(|pixel| pixel[3] == 0).count() as u64;
    let opaque = image.pixels().filter(|pixel| pixel[3] == u8::MAX).count() as u64;
    if transparent * 10 < pixels || opaque * 4 < pixels {
        return Err(format!(
            "unexpected alpha coverage: transparent={transparent}, opaque={opaque}, pixels={pixels}"
        )
        .into());
    }
    Ok(())
}

fn validate_ico(path: &Path) -> Result<(), Box<dyn Error>> {
    let directory = IconDir::read(File::open(path)?)?;
    let actual = directory
        .entries()
        .iter()
        .map(IconDirEntry::width)
        .collect::<Vec<_>>();
    if actual != ICON_SIZES {
        return Err(format!("ICO sizes differ: expected {ICON_SIZES:?}, actual {actual:?}").into());
    }
    if fs::metadata(path)?.len() == 0 {
        return Err("ICO output is empty".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_background_is_removed_but_enclosed_white_is_preserved() {
        let mut image = RgbaImage::from_pixel(5, 5, Rgba([244, 244, 244, 255]));
        for x in 1..=3 {
            for y in 1..=3 {
                image.put_pixel(x, y, Rgba([10, 20, 30, 255]));
            }
        }
        image.put_pixel(2, 2, Rgba([250, 250, 250, 255]));

        let extracted = extract_edge_background(image);
        assert_eq!(extracted.get_pixel(0, 0)[3], 0);
        assert_eq!(extracted.get_pixel(2, 2), &Rgba([250, 250, 250, 255]));
    }

    #[test]
    fn existing_transparency_is_preserved() {
        let image = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 0]));
        assert_eq!(extract_edge_background(image.clone()), image);
    }

    #[test]
    fn transparent_padding_is_trimmed_without_clipping_content() {
        let mut image = RgbaImage::from_pixel(100, 100, Rgba([0, 0, 0, 0]));
        for x in 25..75 {
            for y in 20..80 {
                image.put_pixel(x, y, Rgba([10, 20, 30, 255]));
            }
        }
        let trimmed = trim_transparent_padding(&image);
        assert!(trimmed.width() < image.width());
        assert!(trimmed.height() < image.height());
        assert_eq!(alpha_bounds(&trimmed), Some((2, 2, 51, 61)));
    }

    #[test]
    fn installer_wizard_panel_has_inno_aspect_ratio() {
        let icon = RgbaImage::from_pixel(32, 32, Rgba([20, 40, 80, 255]));
        let panel = wizard_panel(
            &icon,
            Rgba([240, 245, 255, 255]),
            Rgba([210, 225, 250, 255]),
        );
        assert_eq!(panel.dimensions(), WIZARD_IMAGE_SIZE);
    }
}
