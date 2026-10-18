//! Local, offline text extraction for screenshots.
//!
//! Phorminx runs the PaddleOCR PP-OCR detection and Latin recognition models
//! (ONNX) with the pure-Rust RTen runtime. No operating-system OCR service,
//! native inference library, or network access is involved: the caller passes
//! model files that were downloaded and verified like the speech models.
//!
//! The pipeline is the classic two-stage PaddleOCR one: a DB text detector
//! produces a probability map that is turned into line boxes, and each box is
//! cropped, resized to the recognizer height and decoded with greedy CTC.

use std::path::Path;

use image::imageops::{self, FilterType};
use image::{DynamicImage, GenericImageView, RgbImage};
use rten::Model;
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, NdTensorView};

/// Images larger than this many pixels are refused instead of decoded.
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;
/// The detector input is capped at this side length (in pixels) to bound CPU
/// time; screenshots below it are processed at native resolution.
const DETECTOR_MAX_SIDE: u32 = 2_048;
const DETECTOR_BINARY_THRESHOLD: f32 = 0.3;
const DETECTOR_BOX_THRESHOLD: f32 = 0.6;
const DETECTOR_UNCLIP_RATIO: f32 = 1.5;
const RECOGNIZER_DEFAULT_HEIGHT: u32 = 48;
const RECOGNIZER_MAX_WIDTH: u32 = 3_200;
const MAX_LINES: usize = 600;

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("the text-recognition model could not be loaded")]
    Model,
    #[error("the text-recognition dictionary is invalid")]
    Dictionary,
    #[error("the image could not be decoded")]
    Decode,
    #[error("the image is too large to read")]
    TooLarge,
    #[error("text recognition failed")]
    Inference,
}

/// One recognised line, in original-image pixel coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrLine {
    pub text: String,
    /// Mean per-character probability in `0.0..=1.0`.
    pub confidence: f32,
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// A loaded detector/recognizer pair. Loading is comparatively slow; keep one
/// engine per worker and reuse it.
pub struct OcrEngine {
    detector: Model,
    recognizer: Model,
    alphabet: Vec<String>,
    recognizer_height: u32,
}

impl OcrEngine {
    pub fn load(detector: &Path, recognizer: &Path, dictionary: &Path) -> Result<Self, OcrError> {
        let alphabet = std::fs::read_to_string(dictionary)
            .map_err(|_| OcrError::Dictionary)
            .and_then(|text| parse_dictionary(&text))?;
        let detector = Model::load_file(detector).map_err(|_| OcrError::Model)?;
        let recognizer = Model::load_file(recognizer).map_err(|_| OcrError::Model)?;
        let recognizer_height = recognizer
            .input_shape(0)
            .and_then(|shape| match shape.get(2) {
                Some(rten::Dimension::Fixed(height)) if (16..=128).contains(height) => {
                    Some(*height as u32)
                }
                _ => None,
            })
            .unwrap_or(RECOGNIZER_DEFAULT_HEIGHT);
        Ok(Self {
            detector,
            recognizer,
            alphabet,
            recognizer_height,
        })
    }

    /// Decodes an encoded image (PNG, JPEG, …) and reads its text.
    pub fn recognize_encoded(&self, bytes: &[u8]) -> Result<Vec<OcrLine>, OcrError> {
        self.recognize(&decode_image(bytes)?)
    }

    /// Reads all text lines, ordered top-to-bottom, then left-to-right.
    pub fn recognize(&self, image: &RgbImage) -> Result<Vec<OcrLine>, OcrError> {
        if image.width() < 4 || image.height() < 4 {
            return Ok(Vec::new());
        }
        let boxes = self.detect(image)?;
        let mut lines = Vec::new();
        for row in group_rows(boxes) {
            let mut texts = Vec::new();
            let mut confidence = 0.0;
            for region in &row {
                let (text, score) = self.read_region(image, *region)?;
                if !text.trim().is_empty() {
                    texts.push(text.trim().to_owned());
                    confidence += score;
                }
            }
            if texts.is_empty() {
                continue;
            }
            let left = row.iter().map(|r| r.left).min().unwrap_or(0);
            let top = row.iter().map(|r| r.top).min().unwrap_or(0);
            let right = row.iter().map(|r| r.left + r.width).max().unwrap_or(0);
            let bottom = row.iter().map(|r| r.top + r.height).max().unwrap_or(0);
            lines.push(OcrLine {
                confidence: confidence / texts.len() as f32,
                text: texts.join(" "),
                left,
                top,
                width: right - left,
                height: bottom - top,
            });
        }
        Ok(lines)
    }

    fn detect(&self, image: &RgbImage) -> Result<Vec<Region>, OcrError> {
        let (width, height) = image.dimensions();
        let scale = (DETECTOR_MAX_SIDE as f32 / width.max(height) as f32).min(1.0);
        let target_width = round_to_32((width as f32 * scale).round() as u32);
        let target_height = round_to_32((height as f32 * scale).round() as u32);
        let resized = imageops::resize(image, target_width, target_height, FilterType::Triangle);
        // PaddleOCR feeds OpenCV's BGR channel order with ImageNet statistics.
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
        let (w, h) = (target_width as usize, target_height as usize);
        let mut input = NdTensor::<f32, 4>::zeros([1, 3, h, w]);
        for (x, y, pixel) in resized.enumerate_pixels() {
            let bgr = [pixel[2], pixel[1], pixel[0]];
            for channel in 0..3 {
                input[[0, channel, y as usize, x as usize]] =
                    (bgr[channel] as f32 / 255.0 - MEAN[channel]) / STD[channel];
            }
        }
        let output = self
            .detector
            .run_one(input.view().into(), None)
            .map_err(|_| OcrError::Inference)?;
        let map: NdTensor<f32, 4> = output.try_into().map_err(|_| OcrError::Inference)?;
        let [_, _, map_h, map_w] = map.shape();
        let probability = map.slice((0, 0));
        let scale_x = width as f32 / map_w as f32;
        let scale_y = height as f32 / map_h as f32;
        Ok(regions_from_probability(probability, map_w, map_h)
            .into_iter()
            .filter_map(|r| r.scaled(scale_x, scale_y, width, height))
            .collect())
    }

    fn read_region(&self, image: &RgbImage, region: Region) -> Result<(String, f32), OcrError> {
        let mut crop =
            imageops::crop_imm(image, region.left, region.top, region.width, region.height)
                .to_image();
        if crop.height() as f32 >= crop.width() as f32 * 1.5 {
            crop = imageops::rotate270(&crop);
        }
        let target_height = self.recognizer_height;
        let target_width = ((crop.width() as f32 * target_height as f32 / crop.height() as f32)
            .ceil() as u32)
            .clamp(target_height / 2, RECOGNIZER_MAX_WIDTH);
        let resized = imageops::resize(&crop, target_width, target_height, FilterType::Triangle);
        let (w, h) = (target_width as usize, target_height as usize);
        let mut input = NdTensor::<f32, 4>::zeros([1, 3, h, w]);
        for (x, y, pixel) in resized.enumerate_pixels() {
            let bgr = [pixel[2], pixel[1], pixel[0]];
            for channel in 0..3 {
                input[[0, channel, y as usize, x as usize]] = bgr[channel] as f32 / 127.5 - 1.0;
            }
        }
        let output = self
            .recognizer
            .run_one(input.view().into(), None)
            .map_err(|_| OcrError::Inference)?;
        let scores: NdTensor<f32, 3> = output.try_into().map_err(|_| OcrError::Inference)?;
        Ok(ctc_decode(scores.slice(0), &self.alphabet))
    }
}

/// Decodes and bounds an encoded image without trusting its header sizes.
pub fn decode_image(bytes: &[u8]) -> Result<RgbImage, OcrError> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| OcrError::Decode)?;
    let (width, height) = reader.into_dimensions().map_err(|_| OcrError::Decode)?;
    ensure_bounded(width, height)?;
    let image = image::load_from_memory(bytes).map_err(|_| OcrError::Decode)?;
    Ok(flatten_alpha(&image))
}

fn ensure_bounded(width: u32, height: u32) -> Result<(), OcrError> {
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(OcrError::TooLarge);
    }
    Ok(())
}

/// Screenshots with transparency are composited over white, as a viewer would.
fn flatten_alpha(image: &DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let (width, height) = image.dimensions();
    let rgba = image.to_rgba8();
    RgbImage::from_fn(width, height, |x, y| {
        let pixel = rgba.get_pixel(x, y);
        let alpha = pixel[3] as u32;
        let blend = |c: u8| ((c as u32 * alpha + 255 * (255 - alpha)) / 255) as u8;
        image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])])
    })
}

fn parse_dictionary(text: &str) -> Result<Vec<String>, OcrError> {
    let alphabet: Vec<String> = text
        .lines()
        .map(|line| line.trim_end_matches('\r').to_owned())
        .collect();
    if alphabet.is_empty() || alphabet.len() > 50_000 {
        return Err(OcrError::Dictionary);
    }
    Ok(alphabet)
}

fn round_to_32(value: u32) -> u32 {
    (value.div_ceil(32) * 32).max(32)
}

/// Greedy CTC: index 0 is blank, `1..=len` map to the dictionary, and a
/// trailing extra class (when present) is the space character.
fn ctc_decode(scores: NdTensorView<f32, 2>, alphabet: &[String]) -> (String, f32) {
    let [steps, classes] = scores.shape();
    let mut text = String::new();
    let mut total = 0.0;
    let mut count = 0usize;
    let mut previous = 0usize;
    for step in 0..steps {
        let (mut best, mut best_score) = (0usize, f32::MIN);
        for class in 0..classes {
            let score = scores[[step, class]];
            if score > best_score {
                best = class;
                best_score = score;
            }
        }
        if best != 0 && best != previous {
            if let Some(symbol) = alphabet.get(best - 1) {
                text.push_str(symbol);
            } else if best == alphabet.len() + 1 {
                text.push(' ');
            }
            total += best_score;
            count += 1;
        }
        previous = best;
    }
    (
        text,
        if count == 0 {
            0.0
        } else {
            total / count as f32
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Region {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Debug)]
struct MapBox {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl MapBox {
    fn scaled(self, sx: f32, sy: f32, max_w: u32, max_h: u32) -> Option<Region> {
        let left = (self.left * sx).floor().clamp(0.0, max_w as f32) as u32;
        let top = (self.top * sy).floor().clamp(0.0, max_h as f32) as u32;
        let right = (self.right * sx).ceil().clamp(0.0, max_w as f32) as u32;
        let bottom = (self.bottom * sy).ceil().clamp(0.0, max_h as f32) as u32;
        (right > left + 2 && bottom > top + 2).then_some(Region {
            left,
            top,
            width: right - left,
            height: bottom - top,
        })
    }
}

/// DB post-processing with axis-aligned boxes: threshold, 8-connected
/// components, mean-probability filter, then the standard unclip expansion.
fn regions_from_probability(map: NdTensorView<f32, 2>, width: usize, height: usize) -> Vec<MapBox> {
    let mut label = vec![false; width * height];
    let mut boxes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..width * height {
        let (sx, sy) = (start % width, start / width);
        if label[start] || map[[sy, sx]] <= DETECTOR_BINARY_THRESHOLD {
            continue;
        }
        label[start] = true;
        stack.push(start);
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (sx, sy, sx, sy);
        let (mut sum, mut pixels) = (0.0f32, 0usize);
        while let Some(index) = stack.pop() {
            let (x, y) = (index % width, index / width);
            sum += map[[y, x]];
            pixels += 1;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            for (nx, ny) in neighbours(x, y, width, height) {
                let neighbour = ny * width + nx;
                if !label[neighbour] && map[[ny, nx]] > DETECTOR_BINARY_THRESHOLD {
                    label[neighbour] = true;
                    stack.push(neighbour);
                }
            }
        }
        let box_w = (max_x - min_x + 1) as f32;
        let box_h = (max_y - min_y + 1) as f32;
        if box_w.min(box_h) < 3.0 || sum / (pixels as f32) < DETECTOR_BOX_THRESHOLD {
            continue;
        }
        let distance = box_w * box_h * DETECTOR_UNCLIP_RATIO / (2.0 * (box_w + box_h));
        boxes.push(MapBox {
            left: min_x as f32 - distance,
            top: min_y as f32 - distance,
            right: max_x as f32 + 1.0 + distance,
            bottom: max_y as f32 + 1.0 + distance,
        });
        if boxes.len() >= MAX_LINES {
            break;
        }
    }
    boxes
}

fn neighbours(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> impl Iterator<Item = (usize, usize)> {
    let xs = x.saturating_sub(1)..=(x + 1).min(width - 1);
    xs.flat_map(move |nx| (y.saturating_sub(1)..=(y + 1).min(height - 1)).map(move |ny| (nx, ny)))
        .filter(move |&(nx, ny)| (nx, ny) != (x, y))
}

/// Groups boxes into reading-order lines: boxes join a line when their
/// vertical centres fall inside each other's span and the horizontal gap is
/// small relative to the text height, so separate columns (a sidebar beside a
/// message body) stay separate lines.
fn group_rows(mut boxes: Vec<Region>) -> Vec<Vec<Region>> {
    boxes.sort_by_key(|r| (r.left, r.top));
    let mut rows: Vec<Vec<Region>> = Vec::new();
    for region in boxes {
        let centre = region.top + region.height / 2;
        let joined = rows.iter_mut().find(|row| {
            let last = row[row.len() - 1];
            let top = row.iter().map(|r| r.top).min().unwrap_or(0);
            let bottom = row.iter().map(|r| r.top + r.height).max().unwrap_or(0);
            let row_centre = (top + bottom) / 2;
            let gap = region.left.saturating_sub(last.left + last.width);
            (top..bottom).contains(&centre)
                && (region.top..region.top + region.height).contains(&row_centre)
                && gap <= (bottom - top) * 2
        });
        match joined {
            Some(row) => row.push(region),
            None => rows.push(vec![region]),
        }
    }
    rows.sort_by_key(|row| {
        let top = row.iter().map(|r| r.top).min().unwrap_or(0);
        let bottom = row.iter().map(|r| r.top + r.height).max().unwrap_or(0);
        ((top + bottom) / 2, row[0].left)
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alphabet() -> Vec<String> {
        ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn ctc_collapses_repeats_skips_blanks_and_maps_trailing_space() {
        // classes: blank, a, b, c, space
        let steps = [1, 1, 0, 1, 4, 2, 2, 0, 3];
        let mut data = vec![0.0f32; steps.len() * 5];
        for (step, class) in steps.iter().enumerate() {
            data[step * 5 + class] = 0.9;
        }
        let scores = NdTensor::from_data([steps.len(), 5], data);
        let (text, confidence) = ctc_decode(scores.view(), &alphabet());
        assert_eq!(text, "aa bc");
        assert!((confidence - 0.9).abs() < 1e-6);
    }

    #[test]
    fn empty_ctc_output_has_zero_confidence() {
        let scores = NdTensor::from_data(
            [2, 5],
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        );
        assert_eq!(ctc_decode(scores.view(), &alphabet()), (String::new(), 0.0));
    }

    #[test]
    fn components_become_unclipped_boxes_and_weak_regions_are_dropped() {
        let (w, h) = (40, 20);
        let mut data = vec![0.0f32; w * h];
        for y in 5..10 {
            for x in 4..30 {
                data[y * w + x] = 0.9;
            }
        }
        // A faint blob above the binary threshold but below the box score.
        for y in 14..18 {
            for x in 34..38 {
                data[y * w + x] = 0.35;
            }
        }
        let map = NdTensor::from_data([h, w], data);
        let boxes = regions_from_probability(map.view(), w, h);
        assert_eq!(boxes.len(), 1);
        let expected = 26.0 * 5.0 * 1.5 / (2.0 * 31.0);
        assert!((boxes[0].left - (4.0 - expected)).abs() < 1e-4);
        assert!((boxes[0].bottom - (10.0 + expected)).abs() < 1e-4);
    }

    #[test]
    fn rows_group_side_by_side_boxes_in_reading_order() {
        let region = |left, top| Region {
            left,
            top,
            width: 30,
            height: 12,
        };
        let rows = group_rows(vec![
            region(50, 41),
            region(10, 40),
            region(10, 10),
            region(300, 40),
        ]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec![region(10, 10)]);
        assert_eq!(rows[1], vec![region(10, 40), region(50, 41)]);
        assert_eq!(rows[2], vec![region(300, 40)]);
    }

    #[test]
    fn dictionary_rejects_empty_and_keeps_symbols_verbatim() {
        assert!(parse_dictionary("").is_err());
        assert_eq!(parse_dictionary("á\r\nñ\n").unwrap(), vec!["á", "ñ"]);
    }

    #[test]
    fn oversized_or_corrupt_images_are_refused_before_decoding() {
        assert!(matches!(
            decode_image(b"not an image"),
            Err(OcrError::Decode)
        ));
        assert!(matches!(
            ensure_bounded(100_000, 100_000),
            Err(OcrError::TooLarge)
        ));
        assert!(ensure_bounded(3840, 2160).is_ok());
    }

    #[test]
    fn transparent_pixels_are_composited_over_white() {
        let mut rgba = image::RgbaImage::new(2, 1);
        rgba.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        rgba.put_pixel(1, 0, image::Rgba([0, 0, 0, 255]));
        let flat = flatten_alpha(&DynamicImage::ImageRgba8(rgba));
        assert_eq!(flat.get_pixel(0, 0).0, [255, 255, 255]);
        assert_eq!(flat.get_pixel(1, 0).0, [0, 0, 0]);
    }
}
