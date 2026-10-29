use anyhow::{anyhow, ensure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::{codecs::jpeg::JpegEncoder, GenericImageView, ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Cursor;

const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIMENSION: u32 = 1536;

// Image payloads must never appear in debug output or persisted job records.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiImage {
    pub mime_type: String,
    pub data_base64: String,
}

impl AiImage {
    pub fn from_bytes(mime_type: &str, bytes: &[u8]) -> Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_SOURCE_BYTES,
            "ai_image_invalid"
        );
        let format = match mime_type {
            "image/png" => ImageFormat::Png,
            "image/jpeg" => ImageFormat::Jpeg,
            "image/webp" => ImageFormat::WebP,
            "image/gif" => ImageFormat::Gif,
            _ => return Err(anyhow!("ai_image_invalid")),
        };
        ensure!(
            image::guess_format(bytes).ok() == Some(format),
            "ai_image_invalid"
        );
        let dimensions = ImageReader::with_format(Cursor::new(bytes), format)
            .into_dimensions()
            .map_err(|_| anyhow!("ai_image_invalid"))?;
        ensure!(
            dimensions.0 > 0
                && dimensions.1 > 0
                && dimensions.0 <= 8192
                && dimensions.1 <= 8192
                && u64::from(dimensions.0) * u64::from(dimensions.1) <= 16 * 1024 * 1024,
            "ai_image_invalid"
        );
        let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits = Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(96 * 1024 * 1024);
        reader.limits(limits);
        let mut decoder = reader
            .into_decoder()
            .map_err(|_| anyhow!("ai_image_invalid"))?;
        let orientation = image::ImageDecoder::orientation(&mut decoder)
            .map_err(|_| anyhow!("ai_image_invalid"))?;
        let mut image =
            image::DynamicImage::from_decoder(decoder).map_err(|_| anyhow!("ai_image_invalid"))?;
        image.apply_orientation(orientation);
        let image = if image.width() > MAX_DIMENSION || image.height() > MAX_DIMENSION {
            image.resize(
                MAX_DIMENSION,
                MAX_DIMENSION,
                image::imageops::FilterType::Triangle,
            )
        } else {
            image
        };
        let mut encoded = Vec::new();
        // Decoding and encoding strips EXIF, comments and other input metadata.
        let mime_type = if image.color().has_alpha() {
            image
                .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Png)
                .map_err(|_| anyhow!("ai_image_invalid"))?;
            "image/png"
        } else {
            JpegEncoder::new_with_quality(&mut encoded, 85)
                .encode_image(&image.to_rgb8())
                .map_err(|_| anyhow!("ai_image_invalid"))?;
            "image/jpeg"
        };
        ensure!(encoded.len() <= MAX_IMAGE_BYTES, "ai_image_invalid");
        Ok(Self {
            mime_type: mime_type.to_owned(),
            data_base64: STANDARD.encode(encoded),
        })
    }

    pub fn normalized(&self) -> Result<Self> {
        ensure!(
            self.data_base64.len() <= MAX_IMAGE_BYTES.div_ceil(3) * 4,
            "ai_image_invalid"
        );
        let bytes = STANDARD
            .decode(&self.data_base64)
            .map_err(|_| anyhow!("ai_image_invalid"))?;
        ensure!(bytes.len() <= MAX_IMAGE_BYTES, "ai_image_invalid");
        Self::from_bytes(&self.mime_type, &bytes)
    }

    pub fn evidence_ref(&self) -> String {
        // The caller uses the normalized payload in both the snapshot and request.
        let bytes = STANDARD.decode(&self.data_base64).unwrap_or_default();
        format!("i-{}", hex::encode(Sha256::digest(bytes)))
    }
}

pub fn normalize_images(images: &[AiImage]) -> Result<Vec<AiImage>> {
    ensure!(images.len() <= 4, "ai_image_invalid");
    images.iter().map(AiImage::normalized).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phone_photo_orientation_is_applied_before_metadata_is_removed() {
        let mut jpeg = vec![];
        image::DynamicImage::new_rgb8(3, 2)
            .write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
            .unwrap();
        let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut oriented = vec![0xff, 0xd8, 0xff, 0xe1];
        oriented.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        oriented.extend_from_slice(exif);
        oriented.extend_from_slice(&jpeg[2..]);
        let normalized = AiImage::from_bytes("image/jpeg", &oriented).unwrap();
        let bytes = STANDARD.decode(&normalized.data_base64).unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (2, 3));
        assert!(!bytes.windows(6).any(|window| window == b"Exif\0\0"));
    }
    #[test]
    fn image_decode_normalizes_dimensions_and_rejects_mime_or_corruption() {
        let image = image::DynamicImage::new_rgba8(1600, 2);
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes.extend_from_slice(b"synthetic-private-metadata");
        let normalized = AiImage::from_bytes("image/png", &bytes).unwrap();
        let decoded = STANDARD.decode(&normalized.data_base64).unwrap();
        assert!(!decoded
            .windows(b"synthetic-private-metadata".len())
            .any(|w| w == b"synthetic-private-metadata"));
        let dimensions = image::load_from_memory(&decoded).unwrap().dimensions();
        assert_eq!(dimensions.0, 1536);
        assert!((1..=2).contains(&dimensions.1));
        assert!(AiImage::from_bytes("image/jpeg", &bytes).is_err());
        assert!(AiImage::from_bytes("image/png", &bytes[..16]).is_err());
        let mut large_source = bytes.clone();
        large_source.resize(MAX_IMAGE_BYTES + 1, 0);
        assert!(AiImage::from_bytes("image/png", &large_source).is_ok());
        assert!(AiImage {
            mime_type: "image/png".into(),
            data_base64: STANDARD.encode(&large_source)
        }
        .normalized()
        .is_err());
        large_source.resize(MAX_SOURCE_BYTES + 1, 0);
        assert!(AiImage::from_bytes("image/png", &large_source).is_err());
        let mut webp = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut webp), ImageFormat::WebP)
            .unwrap();
        let from_webp = AiImage::from_bytes("image/webp", &webp).unwrap();
        assert_eq!(from_webp.mime_type, "image/png");
        assert!(image::load_from_memory(&STANDARD.decode(&from_webp.data_base64).unwrap()).is_ok());
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            encoder
                .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                    2,
                    2,
                    image::Rgba([255, 0, 0, 255]),
                )))
                .unwrap();
            encoder
                .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                    2,
                    2,
                    image::Rgba([0, 0, 255, 255]),
                )))
                .unwrap();
        }
        let first = AiImage::from_bytes("image/gif", &gif).unwrap();
        assert!(matches!(
            first.mime_type.as_str(),
            "image/png" | "image/jpeg"
        ));
        let first_bytes = STANDARD.decode(&first.data_base64).unwrap();
        assert!(matches!(
            image::guess_format(&first_bytes).unwrap(),
            ImageFormat::Png | ImageFormat::Jpeg
        ));
        let pixel = image::load_from_memory(&first_bytes)
            .unwrap()
            .to_rgba8()
            .get_pixel(0, 0)
            .0;
        assert!(pixel[0] > 240 && pixel[1] < 15 && pixel[2] < 15 && pixel[3] == 255);
        assert!(normalize_images(&vec![normalized; 5]).is_err());
    }
}
