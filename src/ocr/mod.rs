//! OCR 抽象層。目前唯一實作為 Windows.Media.Ocr,
//! trait 隔離讓之後可換 Tesseract 等引擎。

mod win_ocr;

pub use win_ocr::WinOcr;

pub trait OcrBackend {
    /// 辨識一張 BGRA 影像,回傳整行文字。
    fn recognize_bgra(&mut self, w: u32, h: u32, bgra: &[u8]) -> anyhow::Result<String>;
}
