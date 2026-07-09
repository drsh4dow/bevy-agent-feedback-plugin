use super::{AgentClient, Capture, ClientError, OcrOptions, Region};
use crate::session::unix_ms;
use image::GenericImageView;
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_IGNORE_REGIONS: usize = 8;

pub(super) fn image_error(error: image::ImageError) -> ClientError {
    ClientError::Assertion(error.to_string())
}

fn diff_images(
    a: &image::DynamicImage,
    b: &image::DynamicImage,
    include: Option<Region>,
    masks: &[Region],
) -> Result<u64, ClientError> {
    if a.dimensions() != b.dimensions() {
        return Err(ClientError::Assertion(format!(
            "image dimensions differ: {:?} vs {:?} (window resized; re-capture physical regions)",
            a.dimensions(),
            b.dimensions()
        )));
    }
    if masks.len() > MAX_IGNORE_REGIONS {
        return Err(ClientError::Assertion(format!(
            "at most {MAX_IGNORE_REGIONS} ignore regions are allowed, got {}",
            masks.len()
        )));
    }
    let include = include.unwrap_or(Region {
        x: 0,
        y: 0,
        width: a.width(),
        height: a.height(),
    });
    validate_region(a, include)?;
    validate_region(b, include)?;
    for &mask in masks {
        validate_region(a, mask)?;
        validate_region(b, mask)?;
    }

    let x_end = include.x + include.width;
    let y_end = include.y + include.height;
    let mut changed = 0;
    for y in include.y..y_end {
        for x in include.x..x_end {
            if masks.iter().any(|mask| region_contains(*mask, x, y)) {
                continue;
            }
            if a.get_pixel(x, y) != b.get_pixel(x, y) {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

fn region_contains(region: Region, x: u32, y: u32) -> bool {
    x >= region.x && x < region.x + region.width && y >= region.y && y < region.y + region.height
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

fn validate_filters(
    path: &Path,
    include: Option<Region>,
    masks: &[Region],
) -> Result<(), ClientError> {
    if masks.len() > MAX_IGNORE_REGIONS {
        return Err(ClientError::Assertion(format!(
            "at most {MAX_IGNORE_REGIONS} ignore regions are allowed, got {}",
            masks.len()
        )));
    }
    let image = image::ImageReader::open(path)?
        .decode()
        .map_err(image_error)?;
    if let Some(include) = include {
        validate_region(&image, include)?;
    }
    for &mask in masks {
        validate_region(&image, mask)?;
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
    let _ = child.wait_with_output();
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

impl AgentClient {
    /// Counts differing physical PNG pixels across two equally-sized images.
    pub fn pixel_diff(a: impl AsRef<Path>, b: impl AsRef<Path>) -> Result<u64, ClientError> {
        Self::pixel_diff_filtered(a, b, None, &[])
    }

    /// Counts differing physical PNG pixels inside `include`, excluding up to eight masks.
    pub fn pixel_diff_filtered(
        a: impl AsRef<Path>,
        b: impl AsRef<Path>,
        include: Option<Region>,
        masks: &[Region],
    ) -> Result<u64, ClientError> {
        let a = image::ImageReader::open(a)?.decode().map_err(image_error)?;
        let b = image::ImageReader::open(b)?.decode().map_err(image_error)?;
        diff_images(&a, &b, include, masks)
    }

    /// Counts differing pixels inside a physical PNG region.
    pub fn region_diff(
        a: impl AsRef<Path>,
        b: impl AsRef<Path>,
        region: Region,
    ) -> Result<u64, ClientError> {
        Self::pixel_diff_filtered(a, b, Some(region), &[])
    }

    /// Asserts that two screenshots differ by at least `min_pixels`.
    pub fn assert_changed(
        a: impl AsRef<Path>,
        b: impl AsRef<Path>,
        min_pixels: u64,
    ) -> Result<(), ClientError> {
        Self::assert_changed_filtered(a, b, min_pixels, None, &[])
    }

    /// Asserts a filtered physical-pixel difference threshold.
    pub fn assert_changed_filtered(
        a: impl AsRef<Path>,
        b: impl AsRef<Path>,
        min_pixels: u64,
        include: Option<Region>,
        masks: &[Region],
    ) -> Result<(), ClientError> {
        let changed = Self::pixel_diff_filtered(&a, &b, include, masks)?;
        if changed >= min_pixels {
            return Ok(());
        }
        Err(ClientError::Assertion(format!(
            "screenshots changed {changed} pixels, expected at least {min_pixels} \
             (include={include:?}, masks={masks:?}): {} and {}",
            a.as_ref().display(),
            b.as_ref().display()
        )))
    }

    /// Asserts that two screenshots differ inside `region`.
    pub fn assert_region_changed(
        a: impl AsRef<Path>,
        b: impl AsRef<Path>,
        region: Region,
        min_pixels: u64,
    ) -> Result<(), ClientError> {
        Self::assert_changed_filtered(a, b, min_pixels, Some(region), &[])
    }

    /// Asserts that a color appears at least `min_pixels` times.
    pub fn assert_color_present(
        path: impl AsRef<Path>,
        color: [u8; 3],
        region: Option<Region>,
        tolerance: u8,
        min_pixels: u64,
    ) -> Result<(), ClientError> {
        let found = color_pixel_count(path.as_ref(), color, region, tolerance)?;
        if found >= min_pixels {
            return Ok(());
        }
        Err(ClientError::Assertion(format!(
            "color {color:?} found {found} pixels, expected at least {min_pixels}: {}",
            path.as_ref().display()
        )))
    }

    /// Captures atomically until the screenshot differs from `before`.
    pub fn wait_until_changed(
        &mut self,
        before: impl AsRef<Path>,
        frames_per_poll: u16,
        attempts: u16,
    ) -> Result<Capture, ClientError> {
        self.wait_until_changed_filtered(before, frames_per_poll, attempts, None, &[])
    }

    /// Captures atomically until the filtered physical PNG pixels change.
    pub fn wait_until_changed_filtered(
        &mut self,
        before: impl AsRef<Path>,
        frames_per_poll: u16,
        attempts: u16,
        include: Option<Region>,
        masks: &[Region],
    ) -> Result<Capture, ClientError> {
        for _ in 0..attempts {
            let capture = self.capture_after_frames(frames_per_poll, None)?;
            let dimensions = image::image_dimensions(before.as_ref())
                .map_err(|error| ClientError::Assertion(error.to_string()))?;
            if dimensions != (capture.image_width, capture.image_height) {
                validate_filters(&capture.path, include, masks)?;
                return Ok(capture);
            }
            if Self::pixel_diff_filtered(before.as_ref(), &capture.path, include, masks)? > 0 {
                return Ok(capture);
            }
        }
        Err(ClientError::Assertion(self.visual_timeout(
            "screenshot did not change",
            attempts,
            frames_per_poll,
            include,
            masks,
        )))
    }

    /// Waits for strict whole-image pixel stability.
    pub fn wait_until_stable(
        &mut self,
        frames_per_poll: u16,
        attempts: u16,
        stable_polls: u16,
    ) -> Result<Capture, ClientError> {
        self.wait_until_stable_filtered(frames_per_poll, attempts, stable_polls, None, &[])
    }

    /// Waits for strict filtered physical PNG pixel stability.
    pub fn wait_until_stable_filtered(
        &mut self,
        frames_per_poll: u16,
        attempts: u16,
        stable_polls: u16,
        include: Option<Region>,
        masks: &[Region],
    ) -> Result<Capture, ClientError> {
        let stable_polls = stable_polls.max(1);
        let mut previous = self.capture_after_frames(frames_per_poll, None)?;
        let mut streak = 0u16;
        for _ in 0..attempts {
            let current = self.capture_after_frames(frames_per_poll, None)?;
            let resized = (previous.image_width, previous.image_height)
                != (current.image_width, current.image_height);
            let changed = if resized {
                validate_filters(&current.path, include, masks)?;
                1
            } else {
                Self::pixel_diff_filtered(&previous.path, &current.path, include, masks)?
            };
            if changed == 0 {
                streak += 1;
                if streak >= stable_polls {
                    return Ok(current);
                }
            } else {
                streak = 0;
            }
            previous = current;
        }
        Err(ClientError::Assertion(self.visual_timeout(
            "screen did not stabilize",
            attempts,
            frames_per_poll,
            include,
            masks,
        )))
    }

    /// Captures atomically until a color appears in a physical PNG region.
    pub fn wait_until_color(
        &mut self,
        color: [u8; 3],
        region: Region,
        tolerance: u8,
        frames_per_poll: u16,
        attempts: u16,
    ) -> Result<Capture, ClientError> {
        for _ in 0..attempts {
            let capture = self.capture_after_frames(frames_per_poll, None)?;
            if image_has_color(&capture.path, color, region, tolerance)? {
                return Ok(capture);
            }
        }
        Err(ClientError::Assertion(self.visual_timeout(
            "color did not appear",
            attempts,
            frames_per_poll,
            Some(region),
            &[],
        )))
    }

    /// Runs Tesseract OCR on an image.
    pub fn ocr_image(&self, path: impl AsRef<Path>) -> Result<String, ClientError> {
        run_tesseract(&self.ocr, path.as_ref())
    }

    /// Crops a physical PNG region and runs Tesseract OCR on it.
    pub fn ocr_region(
        &self,
        path: impl AsRef<Path>,
        region: Region,
    ) -> Result<String, ClientError> {
        let image = image::ImageReader::open(path)?
            .decode()
            .map_err(image_error)?;
        validate_region(&image, region)?;
        let cropped = image.crop_imm(region.x, region.y, region.width, region.height);
        let temp = std::env::temp_dir().join(format!(
            "bevy-feedback-ocr-{}-{}.png",
            std::process::id(),
            unix_ms()
        ));
        cropped.save(&temp).map_err(image_error)?;
        let result = self.ocr_image(&temp);
        let _ = fs::remove_file(temp);
        result
    }

    /// Asserts that OCR output contains `expected`.
    pub fn assert_text(&self, path: impl AsRef<Path>, expected: &str) -> Result<(), ClientError> {
        let text = normalize_text(&self.ocr_image(path)?);
        let expected = normalize_text(expected);
        if text.contains(&expected) {
            Ok(())
        } else {
            Err(ClientError::Assertion(format!(
                "OCR text did not contain '{expected}': {text}"
            )))
        }
    }

    /// Captures atomically until OCR output contains `expected`.
    pub fn wait_until_text(
        &mut self,
        expected: &str,
        frames_per_poll: u16,
        attempts: u16,
    ) -> Result<Capture, ClientError> {
        for _ in 0..attempts {
            let capture = self.capture_after_frames(frames_per_poll, None)?;
            match self.assert_text(&capture.path, expected) {
                Ok(()) => return Ok(capture),
                Err(ClientError::Assertion(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Err(ClientError::Assertion(self.visual_timeout(
            &format!("text '{expected}' did not appear"),
            attempts,
            frames_per_poll,
            None,
            &[],
        )))
    }

    fn visual_timeout(
        &self,
        message: &str,
        attempts: u16,
        frames_per_capture: u16,
        include: Option<Region>,
        masks: &[Region],
    ) -> String {
        let last_capture_path = self
            .last_capture
            .as_ref()
            .map(|capture| capture.path.display().to_string())
            .unwrap_or_else(|| "<none>".to_string());
        format!(
            "{message}; attempts={attempts}, frames_per_capture={frames_per_capture}, \
             include={include:?}, masks={masks:?}, last_capture_path={last_capture_path}"
        )
    }
}
