use super::{ClientError, OcrOptions, Region};
use image::GenericImageView;
use std::{
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) fn image_error(error: image::ImageError) -> ClientError {
    ClientError::Assertion(error.to_string())
}

pub(super) fn diff_images(
    a: &image::DynamicImage,
    b: &image::DynamicImage,
    region: Option<Region>,
) -> Result<u64, ClientError> {
    if a.dimensions() != b.dimensions() {
        return Err(ClientError::Assertion(format!(
            "image dimensions differ: {:?} vs {:?} (window resized; wait_until_stable + re-capture)",
            a.dimensions(),
            b.dimensions()
        )));
    }
    let region = region.unwrap_or(Region {
        x: 0,
        y: 0,
        width: a.width(),
        height: a.height(),
    });
    validate_region(a, region)?;
    let mut changed = 0;
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            if a.get_pixel(x, y) != b.get_pixel(x, y) {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

pub(super) fn image_has_color(
    path: &Path,
    color: [u8; 3],
    region: Region,
    tolerance: u8,
) -> Result<bool, ClientError> {
    Ok(color_pixel_count(path, color, Some(region), tolerance)? > 0)
}

pub(super) fn color_pixel_count(
    path: &Path,
    color: [u8; 3],
    region: Option<Region>,
    tolerance: u8,
) -> Result<u64, ClientError> {
    let image = image::ImageReader::open(path)?
        .decode()
        .map_err(image_error)?;
    let region = region.unwrap_or(Region {
        x: 0,
        y: 0,
        width: image.width(),
        height: image.height(),
    });
    validate_region(&image, region)?;
    let mut found = 0;
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            let pixel = image.get_pixel(x, y);
            if close(pixel[0], color[0], tolerance)
                && close(pixel[1], color[1], tolerance)
                && close(pixel[2], color[2], tolerance)
            {
                found += 1;
            }
        }
    }
    Ok(found)
}

pub(super) fn validate_region(
    image: &image::DynamicImage,
    region: Region,
) -> Result<(), ClientError> {
    let x_end = region
        .x
        .checked_add(region.width)
        .ok_or_else(|| ClientError::Assertion("region x + width overflowed u32".to_string()))?;
    let y_end = region
        .y
        .checked_add(region.height)
        .ok_or_else(|| ClientError::Assertion("region y + height overflowed u32".to_string()))?;
    if region.width == 0 || region.height == 0 || x_end > image.width() || y_end > image.height() {
        return Err(ClientError::Assertion(format!(
            "region {:?} is outside image {}x{}",
            region,
            image.width(),
            image.height()
        )));
    }
    Ok(())
}

fn close(actual: u8, expected: u8, tolerance: u8) -> bool {
    actual.abs_diff(expected) <= tolerance
}

pub(super) fn run_tesseract(options: &OcrOptions, path: &Path) -> Result<String, ClientError> {
    let mut child = Command::new(&options.tesseract)
        .arg(path)
        .arg("stdout")
        .arg("-l")
        .arg(&options.language)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            ClientError::OcrUnavailable(format!(
                "tesseract unavailable at {}: {error}",
                options.tesseract.display()
            ))
        })?;

    let start = Instant::now();
    while start.elapsed() < options.timeout {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child.wait_with_output()?;
                if output.status.success() {
                    return String::from_utf8(output.stdout)
                        .map_err(|error| ClientError::Ocr(error.to_string()));
                }
                return Err(ClientError::OcrUnavailable(
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(ClientError::Ocr(error.to_string())),
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    Err(ClientError::OcrUnavailable(format!(
        "tesseract timed out after {} ms",
        options.timeout.as_millis()
    )))
}

pub(super) fn normalize_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
