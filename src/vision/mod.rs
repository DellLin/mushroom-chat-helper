//! 影像管線執行緒:ROI → 標記 → 切行 → 分類 → 去重 → OCR → 訊息。

pub mod classify;
pub mod dedup;
pub mod lines;
pub mod preprocess;

use std::hash::Hasher;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::config::Config;
use crate::model::{ChatMessage, FramePacket, UiEvent};
use crate::ocr::{OcrBackend, WinOcr};
use crate::textutil;

use classify::{band_stats, dominant_bright_color, label_pixels, labeled_avg_color, Palette, NO_LABEL};
use dedup::{BandCooldown, LruSet, TextDedup};
use lines::fixed_bands;
use preprocess::prepare_for_ocr;

const PREVIEW_MAX_W: usize = 640;

pub fn spawn(
    frame_rx: Receiver<FramePacket>,
    ui_tx: Sender<UiEvent>,
    cfg: Arc<RwLock<Config>>,
    calibrating: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("vision".into())
        .spawn(move || {
            unsafe {
                use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
            }
            run(frame_rx, ui_tx, cfg, calibrating);
        })
        .expect("無法建立影像管線執行緒")
}

fn run(
    frame_rx: Receiver<FramePacket>,
    ui_tx: Sender<UiEvent>,
    cfg: Arc<RwLock<Config>>,
    calibrating: Arc<AtomicBool>,
) {
    let lang = cfg.read().unwrap().ocr_language.clone();
    let mut ocr = create_ocr(&lang, &ui_tx);
    let mut last_ocr_retry = Instant::now();

    let mut last_preview = Instant::now() - Duration::from_secs(1);
    let mut lru = LruSet::new(512);
    let mut text_dedup = TextDedup::new(cfg.read().unwrap().dedup_seconds);
    let mut cooldown = BandCooldown::new(600);
    let mut prev_band_hashes: Vec<u64> = Vec::new();

    while let Ok(pkt) = frame_rx.recv() {
        // ---- 預覽(全視窗,約 2fps) ----
        if last_preview.elapsed() > Duration::from_millis(500) {
            last_preview = Instant::now();
            let (pw, ph, rgba) = make_preview(&pkt);
            let _ = ui_tx.send(UiEvent::Preview {
                frame_w: pkt.width,
                frame_h: pkt.height,
                w: pw,
                h: ph,
                rgba,
            });
        }

        // OCR 引擎重試(語言包可能在執行中被安裝)。
        if ocr.is_none() && last_ocr_retry.elapsed() > Duration::from_secs(30) {
            last_ocr_retry = Instant::now();
            let lang = cfg.read().unwrap().ocr_language.clone();
            ocr = create_ocr(&lang, &ui_tx);
        }

        let cfg_snap = cfg.read().unwrap().clone();
        text_dedup.set_window(cfg_snap.dedup_seconds);

        // ---- ROI 裁切 ----
        let (fw, fh) = (pkt.width as usize, pkt.height as usize);
        let roi = cfg_snap.roi;
        let rx = (roi.x as usize).min(fw.saturating_sub(1));
        let ry = (roi.y as usize).min(fh.saturating_sub(1));
        let rw = (roi.w as usize).min(fw - rx);
        let rh = (roi.h as usize).min(fh - ry);
        if rw < 24 || rh < 10 {
            continue;
        }
        let mut crop = vec![0u8; rw * rh * 4];
        for y in 0..rh {
            let src = ((ry + y) * fw + rx) * 4;
            let dst = y * rw * 4;
            crop[dst..dst + rw * 4].copy_from_slice(&pkt.bgra[src..src + rw * 4]);
        }

        // ---- 標記與切行(固定高度網格) ----
        let palette = Palette::from_channels(&cfg_snap.channels);
        if palette.entries.is_empty() {
            continue;
        }
        let (labels, _rows) = label_pixels(rw, rh, &crop, &palette);
        let bands = fixed_bands(rh, cfg_snap.line_height_px.max(1) as usize);

        let calib = calibrating.load(Ordering::Relaxed);

        if bands.is_empty() {
            prev_band_hashes.clear();
            continue;
        }

        // ---- 整幀快篩:所有 band 的遮罩雜湊與上一幀相同就跳過 ----
        let band_hashes: Vec<u64> = bands
            .iter()
            .map(|b| hash_band_labels(rw, &labels, b.y0, b.y1))
            .collect();
        if band_hashes == prev_band_hashes {
            continue;
        }
        prev_band_hashes = band_hashes.clone();

        // ---- 逐行處理 ----
        for (band, &bhash) in bands.iter().zip(band_hashes.iter()) {
            if !lru.insert_if_new(bhash) {
                continue;
            }
            let stats = band_stats(rw, &labels, band.y0, band.y1, palette.entries.len());
            let dom = stats.dominant(20);

            // 未分類行:非校準模式且未開啟顯示時直接略過(不花 OCR)。
            if dom.is_none() && !cfg_snap.include_unknown && !calib {
                continue;
            }

            let label = dom.map(|d| d as u8).unwrap_or(NO_LABEL);
            if !cooldown.allow(band.y0, label) {
                continue;
            }

            let Some(engine) = ocr.as_mut() else { continue };
            let (ow, oh, obuf) = prepare_for_ocr(
                rw,
                &crop,
                band.y0,
                band.y1,
                &palette,
                dom,
                cfg_snap.ocr_scale.clamp(1, 6) as usize,
            );
            let text = match engine.recognize_bgra(ow, oh, &obuf) {
                Ok(t) => textutil::clean_ocr_text(&t),
                Err(e) => {
                    log::debug!("OCR 失敗: {e}");
                    continue;
                }
            };
            if text.chars().count() < 2 {
                continue;
            }

            if calib {
                // 已分類的行:用分類階段實際標記為此頻道的像素平均色,不受背景面積影響。
                // 未分類的行:退回亮度眾數取樣(沒有「正確頻道」可以鎖定)。
                // 兩者都可能因像素不足取樣失敗,退回灰色佔位,不因此擋住 OCR 畫面本身的除錯用途。
                let color = dom
                    .and_then(|d| labeled_avg_color(rw, &crop, &labels, d as u8, band.y0, band.y1))
                    .or_else(|| dominant_bright_color(rw, &crop, band.y0, band.y1))
                    .unwrap_or([160, 160, 160]);
                let _ = ui_tx.send(UiEvent::CalibSample {
                    color,
                    text: text.clone(),
                    img_w: ow,
                    img_h: oh,
                    img_rgba: obuf,
                });
            }

            if !text_dedup.check_and_insert(&text) {
                continue;
            }

            let channel_id = dom
                .map(|d| cfg_snap.channels[palette.entries[d].0].id.clone())
                .unwrap_or_else(|| "unknown".to_string());
            // 校準模式下仍會 OCR 未分類行以取樣顏色,但未開啟顯示時不送進聊天。
            if channel_id == "unknown" && !cfg_snap.include_unknown {
                continue;
            }
            let stripped = textutil::strip_leading_tag(&text);
            let (sender, content) = textutil::parse_sender(stripped);
            let _ = ui_tx.send(UiEvent::Message(ChatMessage {
                time: chrono::Local::now(),
                channel: channel_id,
                sender,
                content,
            }));
        }
    }
}

fn create_ocr(lang: &str, ui_tx: &Sender<UiEvent>) -> Option<WinOcr> {
    match WinOcr::new(lang) {
        Ok(e) => Some(e),
        Err(e) => {
            let _ = ui_tx.send(UiEvent::Error(format!(
                "OCR 引擎初始化失敗({e})。請確認已安裝「中文(台灣)」語言的 OCR 功能:\
                 設定 → 時間與語言 → 語言與地區 → 中文(台灣) → 語言選項 → 光學字元辨識。\
                 安裝後工具會自動重試。"
            )));
            None
        }
    }
}

/// 對 band 的標記(文字遮罩+頻道)計算雜湊。
/// 半透明背景的變動不影響標記核心,雜湊在行未變動時保持穩定。
fn hash_band_labels(w: usize, labels: &[u8], y0: usize, y1: usize) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(&labels[y0 * w..y1 * w]);
    h.finish()
}

/// 最近鄰縮小 + BGRA→RGBA。
fn make_preview(pkt: &FramePacket) -> (usize, usize, Vec<u8>) {
    let (w, h) = (pkt.width as usize, pkt.height as usize);
    let step = (w + PREVIEW_MAX_W - 1) / PREVIEW_MAX_W;
    let step = step.max(1);
    let pw = w / step;
    let ph = h / step;
    let mut rgba = vec![0u8; pw * ph * 4];
    for y in 0..ph {
        for x in 0..pw {
            let src = (y * step * w + x * step) * 4;
            let dst = (y * pw + x) * 4;
            rgba[dst] = pkt.bgra[src + 2]; // R
            rgba[dst + 1] = pkt.bgra[src + 1]; // G
            rgba[dst + 2] = pkt.bgra[src]; // B
            rgba[dst + 3] = 255;
        }
    }
    (pw, ph, rgba)
}
