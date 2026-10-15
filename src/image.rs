//! An image ready to send: a still PNG or JPEG inside the bounds the service accepts.
//!
//! The bounds are TypeSafe's (docs.typesafe.ai/go/images), and the one that bites is the
//! longest side. A full-resolution iPhone 17 Pro screenshot is 1206×2622, and sent as it is
//! the service answers `400 Invalid image dimensions` — measured through OpenRouter on
//! 2026-10-15. Every screenshot a caller is likely to have is over the limit, so a caller
//! that has to remember to shrink it first is a caller that gets a 400 on its first try.
//! jevi shrinks it. What it will not do is enlarge one: an image too small to read is
//! refused, because upscaling invents pixels the model then reasons about.

use std::io::Cursor;

use image::{imageops::FilterType, ImageFormat, ImageReader};

use crate::error::{Error, Result};

/// The longest side the service takes. Also, squared, its pixel ceiling (2,458,624).
pub const MAX_SIDE: u32 = 1568;
/// The shortest side the service takes.
pub const MIN_SIDE: u32 = 256;
/// The fewest pixels the service takes: a 256×256 image is still refused.
pub const MIN_PIXELS: u64 = 78_400;
/// One file, after any shrinking.
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
}

impl Format {
    pub fn mime(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
        }
    }

    /// The name TypeSafe's `assets.format` takes.
    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpeg",
        }
    }
}

/// One image, named the way the question refers to it: an image called `screen` is the
/// thing a question means by "`screen`".
#[derive(Debug, Clone)]
pub struct Image {
    pub name: String,
    pub format: Format,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Image {
    pub fn open(name: &str, path: &str) -> Result<Image> {
        let bytes =
            std::fs::read(path).map_err(|e| Error::invalid("image", format!("{path}: {e}")))?;
        Image::from_bytes(name, bytes).map_err(|e| match e {
            Error::Invalid { kind, message } => Error::Invalid {
                kind,
                message: format!("{path}: {message}"),
            },
            other => other,
        })
    }

    /// Reads the header, and decodes only when the image has to be shrunk.
    pub fn from_bytes(name: &str, bytes: Vec<u8>) -> Result<Image> {
        check_name(name)?;
        let bad = |m: String| Error::invalid("image", m);

        let reader = ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|e| bad(e.to_string()))?;
        let format = match reader.format() {
            Some(ImageFormat::Png) => Format::Png,
            Some(ImageFormat::Jpeg) => Format::Jpeg,
            Some(other) => {
                return Err(bad(format!(
                    "{other:?} is not accepted; the service takes PNG or JPEG"
                )))
            }
            None => return Err(bad("not a PNG or a JPEG".into())),
        };
        let (width, height) = reader
            .into_dimensions()
            .map_err(|e| bad(format!("unreadable {}: {e}", format.name())))?;

        if width.min(height) < MIN_SIDE || u64::from(width) * u64::from(height) < MIN_PIXELS {
            return Err(bad(format!(
                "{width}×{height} is too small: the service takes at least {MIN_SIDE} px a side \
                 and {MIN_PIXELS} pixels"
            )));
        }

        let image = if width.max(height) > MAX_SIDE {
            shrink(name, &bytes, format)?
        } else {
            Image {
                name: name.to_owned(),
                format,
                bytes,
                width,
                height,
            }
        };

        // Shrinking a very tall or very wide image to fit the longest side can take the
        // shortest one under the floor. Refused, not padded: padding is pixels the model
        // would read as part of the screen.
        if image.width.min(image.height) < MIN_SIDE {
            return Err(bad(format!(
                "{width}×{height} is too elongated: fitting it in {MAX_SIDE} px leaves a side \
                 under {MIN_SIDE} px. Crop it first"
            )));
        }
        if image.bytes.len() > MAX_BYTES {
            return Err(bad(format!(
                "{} bytes is over the service's {MAX_BYTES}-byte limit for one image",
                image.bytes.len()
            )));
        }
        Ok(image)
    }
}

fn shrink(name: &str, bytes: &[u8], format: Format) -> Result<Image> {
    let bad = |m: String| Error::invalid("image", m);
    let decoded = image::load_from_memory(bytes).map_err(|e| bad(e.to_string()))?;
    let small = decoded.resize(MAX_SIDE, MAX_SIDE, FilterType::Triangle);

    let mut out = Vec::new();
    let written = match format {
        Format::Png => small.write_to(&mut Cursor::new(&mut out), ImageFormat::Png),
        // The JPEG encoder takes no alpha channel, and a JPEG never had one to lose.
        Format::Jpeg => image::DynamicImage::ImageRgb8(small.to_rgb8())
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg),
    };
    written.map_err(|e| bad(e.to_string()))?;

    Ok(Image {
        name: name.to_owned(),
        format,
        bytes: out,
        width: small.width(),
        height: small.height(),
    })
}

/// The name becomes a key of the state and the word a question uses for the image, so it
/// is held to what reads unambiguously in both places.
fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(
            "image",
            format!("`{name}` is not a usable image name (letters, digits, `_` and `-`)"),
        ))
    }
}

/// `--image NAME=PATH` or `--image PATH`, in the order given. Unnamed images are called
/// `screen` when there is only one, and `image1`, `image2`… when there are several.
pub fn parse_specs(specs: &[String]) -> Vec<(String, String)> {
    let unnamed = specs.iter().filter(|s| split_spec(s).is_none()).count();
    let mut n = 0;
    specs
        .iter()
        .map(|s| match split_spec(s) {
            Some((name, path)) => (name.to_owned(), path.to_owned()),
            None if unnamed == 1 => ("screen".to_owned(), s.clone()),
            None => {
                n += 1;
                (format!("image{n}"), s.clone())
            }
        })
        .collect()
}

/// A path can contain `=`, so only a prefix that could be a name counts as one.
fn split_spec(spec: &str) -> Option<(&str, &str)> {
    let (name, path) = spec.split_once('=')?;
    check_name(name).ok()?;
    (!path.is_empty()).then_some((name, path))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .expect("encodes");
        out
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([200, 20, 30]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
            .expect("encodes");
        out
    }

    #[test]
    fn an_iphone_screenshot_is_shrunk_to_the_longest_side_the_service_takes() {
        let img = Image::from_bytes("screen", png(1206, 2622)).expect("accepted");
        assert_eq!((img.width, img.height), (721, 1568));
        assert_eq!(img.format, Format::Png);
        let (w, h) = ImageReader::new(Cursor::new(&img.bytes))
            .with_guessed_format()
            .expect("reads")
            .into_dimensions()
            .expect("has a size");
        assert_eq!((w, h), (721, 1568), "the bytes sent are the shrunk ones");
    }

    #[test]
    fn a_jpeg_stays_a_jpeg_when_shrunk() {
        let img = Image::from_bytes("photo", jpeg(3000, 2000)).expect("accepted");
        assert_eq!(img.format, Format::Jpeg);
        assert_eq!((img.width, img.height), (1568, 1045));
    }

    #[test]
    fn an_image_inside_the_bounds_goes_byte_for_byte() {
        let bytes = png(721, 1568);
        let img = Image::from_bytes("screen", bytes.clone()).expect("accepted");
        assert_eq!(img.bytes, bytes);
    }

    #[test]
    fn an_image_too_small_to_read_is_refused_not_enlarged() {
        let e = Image::from_bytes("icon", png(200, 400)).expect_err("refused");
        assert_eq!(e.kind(), "image");
        assert_eq!(e.code(), 5);
        let e = Image::from_bytes("icon", png(256, 256)).expect_err("under the pixel floor");
        assert_eq!(e.kind(), "image");
    }

    #[test]
    fn a_strip_that_would_end_up_under_the_floor_is_refused() {
        let e = Image::from_bytes("strip", png(8000, 300)).expect_err("refused");
        assert!(e.to_string().contains("elongated"), "{e}");
    }

    #[test]
    fn something_that_is_not_an_image_says_so() {
        let e = Image::from_bytes("screen", b"hello".to_vec()).expect_err("refused");
        assert_eq!(e.kind(), "image");
    }

    #[test]
    fn unnamed_images_get_names_a_question_can_use() {
        assert_eq!(
            parse_specs(&["a.png".into()]),
            vec![("screen".into(), "a.png".into())]
        );
        assert_eq!(
            parse_specs(&["before=a.png".into(), "b.png".into(), "c.png".into()]),
            vec![
                ("before".into(), "a.png".into()),
                ("image1".into(), "b.png".into()),
                ("image2".into(), "c.png".into()),
            ]
        );
        assert_eq!(
            parse_specs(&["./shots/a=b.png".into()]),
            vec![("screen".into(), "./shots/a=b.png".into())],
            "a path with `=` in it is still a path"
        );
    }
}
