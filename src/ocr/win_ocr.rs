//! Windows.Media.Ocr 實作。

use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;

use super::OcrBackend;

pub struct WinOcr {
    engine: OcrEngine,
    max_dim: u32,
}

impl WinOcr {
    /// lang_tag 例:"zh-Hant"、"zh-TW"、"en-US"。
    pub fn new(lang_tag: &str) -> anyhow::Result<Self> {
        let engine = match Language::CreateLanguage(&HSTRING::from(lang_tag)) {
            Ok(lang) if OcrEngine::IsLanguageSupported(&lang).unwrap_or(false) => {
                OcrEngine::TryCreateFromLanguage(&lang)?
            }
            _ => {
                // 指定語言不可用 → 退回使用者設定檔語言。
                OcrEngine::TryCreateFromUserProfileLanguages()?
            }
        };
        let max_dim = OcrEngine::MaxImageDimension().unwrap_or(2600);
        Ok(Self { engine, max_dim })
    }
}

impl OcrBackend for WinOcr {
    fn recognize_bgra(&mut self, w: u32, h: u32, bgra: &[u8]) -> anyhow::Result<String> {
        if w == 0 || h == 0 || bgra.len() != (w * h * 4) as usize {
            anyhow::bail!("影像尺寸不正確");
        }
        if w > self.max_dim || h > self.max_dim {
            anyhow::bail!("影像超過 OCR 尺寸上限 {}", self.max_dim);
        }
        let buffer = CryptographicBuffer::CreateFromByteArray(bgra)?;
        let bmp = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            w as i32,
            h as i32,
        )?;
        let result = self.engine.RecognizeAsync(&bmp)?.get()?;
        Ok(result.Text()?.to_string_lossy())
    }
}
