//! 影像管線執行緒:ROI 裁切 → 像素標記 → 依文字顏色分類 → 把畫面送到 UI。
//! ROI 本身就是單一則對話的範圍,不做 OCR/文字辨識,聊天內容以擷取到的
//! 原始畫面顯示。

pub mod classify;
pub mod dedup;

use std::hash::Hasher;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::config::{Config, GateRoi, Roi};
use crate::model::{ChatImage, FramePacket, UiEvent};

use classify::{channel_mask_bits, label_pixels, LabelStats, Palette};
use dedup::{ChannelCooldown, ImageDedup, LruSet};

/// 送到 UI 的全視窗預覽最大寬度(等比縮小)。
const PREVIEW_MAX_W: usize = 640;
/// 預覽更新間隔(約 2fps,只是給使用者對 ROI 用,不需要即時)。
const PREVIEW_INTERVAL: Duration = Duration::from_millis(500);

/// 小於這個尺寸的聊天 ROI 視為使用者還沒設定好,直接忽略。
const MIN_CHAT_ROI: (usize, usize) = (24, 10);

/// 主頻道至少要有這麼多命中像素才算數,擋掉零星雜訊點。
const MIN_DOMINANT_PIXELS: u32 = 20;

/// 畫面去重(ImageDedup)只在命中像素數達到這個下限才啟用。低於這個數字的
/// 畫面(雜訊、極短訊息)指紋不可靠,一律視為新訊息放行,見下方呼叫處說明。
const MIN_DEDUP_PIXELS: u32 = 80;

/// 主畫面判斷範圍內要有這麼多像素命中目標顏色才算「在主畫面」,
/// 避免單一雜訊像素(反鋸齒、壓縮雜訊)造成誤判。
const GATE_MIN_HIT_PIXELS: u32 = 6;

pub fn spawn(
    frame_rx: Receiver<FramePacket>,
    ui_tx: Sender<UiEvent>,
    cfg: Arc<RwLock<Config>>,
    full_frame_req: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("vision".into())
        .spawn(move || {
            unsafe {
                use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
            }
            run(frame_rx, ui_tx, cfg, full_frame_req);
        })
        .expect("無法建立影像管線執行緒")
}

fn run(
    frame_rx: Receiver<FramePacket>,
    ui_tx: Sender<UiEvent>,
    cfg: Arc<RwLock<Config>>,
    full_frame_req: Arc<AtomicBool>,
) {
    let mut last_preview = Instant::now() - PREVIEW_INTERVAL;
    let mut lru = LruSet::new(512);
    let mut cooldown = ChannelCooldown::new(600);
    let mut image_dedup = ImageDedup::new(cfg.read().unwrap().dedup_seconds);
    // 上一幀的標記雜湊,用來整幀快篩;None 表示還沒處理過任何一幀。
    let mut prev_hash: Option<u64> = None;
    // 上一次回報給 UI 的主畫面判斷狀態,None 表示還沒回報過(第一幀一定送一次)。
    let mut prev_gate_open: Option<bool> = None;

    while let Ok(pkt) = frame_rx.recv() {
        // ---- 全解析度單張畫面(UI 的「在畫面上框選 ROI」按下時要求一次)----
        // 不論目前是否在主畫面判斷範圍內都要回應,所以放在 gate 檢查之前。
        if full_frame_req.swap(false, Ordering::Relaxed) {
            let _ = ui_tx.send(UiEvent::FullFrame {
                w: pkt.width,
                h: pkt.height,
                rgba: to_rgba(&pkt.bgra),
            });
        }

        // ---- 預覽(全視窗、縮小)----
        if last_preview.elapsed() > PREVIEW_INTERVAL {
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

        let cfg_snap = cfg.read().unwrap().clone();
        image_dedup.set_window(cfg_snap.dedup_seconds);

        // ---- 主畫面判斷:不在遊戲主畫面(如商城、拍賣)時沒有聊天視窗可判斷,
        // 直接跳過這幀,避免持續套用聊天判斷邏輯而誤判出訊息。停用時一律視為
        // 「在主畫面」,UI 才不會因為這個功能而把聊天視窗擋住。
        let is_gate_open = !cfg_snap.gate.enabled || gate_open(&pkt, &cfg_snap.gate);
        if prev_gate_open != Some(is_gate_open) {
            prev_gate_open = Some(is_gate_open);
            let _ = ui_tx.send(UiEvent::GateState(is_gate_open));
        }
        if !is_gate_open {
            continue;
        }

        let palette = Palette::from_channels(&cfg_snap.channels);
        if palette.entries.is_empty() {
            continue;
        }

        // ---- ROI 裁切(順便轉成 UI 要的 RGBA,省下第二次逐像素掃描)----
        let Some((w, h, rgba)) = crop_rgba(&pkt, cfg_snap.roi, MIN_CHAT_ROI) else {
            continue;
        };
        let labels = label_pixels(&rgba, &palette);

        // ---- 整幀快篩:標記跟上一幀完全相同就跳過。半透明背景的變動不影響
        // 標記,雜湊在文字未變動時保持穩定。
        let hash = hash_labels(&labels);
        if prev_hash == Some(hash) {
            continue;
        }
        prev_hash = Some(hash);
        // 這組標記在最近 512 筆裡出現過(例如來回切換的兩則訊息)也跳過。
        if !lru.insert_if_new(hash) {
            continue;
        }

        // ---- 分類:只依顏色決定頻道,畫面直接擷取、不做任何辨識 ----
        let stats = LabelStats::new(&labels, palette.entries.len());
        // 未分類(沒有任何頻道文字顏色明確命中)一律不顯示。
        let Some(dom) = stats.dominant(MIN_DOMINANT_PIXELS) else {
            continue;
        };
        if !cooldown.allow(dom as u8) {
            continue;
        }

        // ROI 只框住單一則對話,所以只需要判斷「這次擷取到的畫面跟上一次是不是
        // 同一則」——把畫面依「這則訊息所屬頻道」的文字顏色過濾成二值遮罩,再用
        // 像素級的交集/聯集比例(IoU)模糊比對,避免同一則訊息因背景抖動重新
        // 觸發而在聊天室裡重複顯示。特意不用 rgba 原始像素比對,因為那包含會
        // 一直變動的半透明背景,會讓靜止不動的文字被誤判成新內容。命中像素太
        // 少(雜訊)時直接跳過比對一律放行,避免長期累積的背景雜訊被反覆判定
        // 成「同一則」而擋掉真正的新訊息。
        if stats.total >= MIN_DEDUP_PIXELS
            && !image_dedup.check_and_insert(channel_mask_bits(&labels, dom as u8))
        {
            continue;
        }

        let _ = ui_tx.send(UiEvent::Message(ChatImage {
            time: chrono::Local::now(),
            channel: cfg_snap.channels[palette.entries[dom].0].id.clone(),
            w: w as u32,
            h: h as u32,
            rgba,
        }));
    }
}

/// 把 ROI 夾進畫面範圍內,回傳 (x, y, w, h);夾完小於 `min` 就回傳 None。
fn clamp_roi(
    roi: Roi,
    fw: usize,
    fh: usize,
    min: (usize, usize),
) -> Option<(usize, usize, usize, usize)> {
    let x = (roi.x as usize).min(fw);
    let y = (roi.y as usize).min(fh);
    let w = (roi.w as usize).min(fw - x);
    let h = (roi.h as usize).min(fh - y);
    (w >= min.0 && h >= min.1).then_some((x, y, w, h))
}

/// 判斷目前畫面是否在遊戲主畫面(判斷範圍內是否存在指定顏色)。
fn gate_open(pkt: &FramePacket, gate: &GateRoi) -> bool {
    let (fw, fh) = (pkt.width as usize, pkt.height as usize);
    let Some((rx, ry, rw, rh)) = clamp_roi(gate.roi, fw, fh, (1, 1)) else {
        return false;
    };
    let tol = gate.tolerance as i32;
    let [tr, tg, tb] = gate.color;
    let mut hits = 0u32;
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            let o = (y * fw + x) * 4;
            let (b, g, r) = (pkt.bgra[o], pkt.bgra[o + 1], pkt.bgra[o + 2]);
            if (r as i32 - tr as i32).abs() <= tol
                && (g as i32 - tg as i32).abs() <= tol
                && (b as i32 - tb as i32).abs() <= tol
            {
                hits += 1;
                if hits >= GATE_MIN_HIT_PIXELS {
                    return true;
                }
            }
        }
    }
    false
}

/// 對標記計算雜湊,用來判斷這一幀的文字內容跟上一幀是否完全相同。
fn hash_labels(labels: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(labels);
    h.finish()
}

/// BGRA → RGBA(alpha 一律補滿),整張畫面、不縮放。
fn to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        out.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    out
}

/// 裁切出 ROI 並轉成 RGBA,回傳 (w, h, rgba)。ROI 太小就回傳 None。
fn crop_rgba(pkt: &FramePacket, roi: Roi, min: (usize, usize)) -> Option<(usize, usize, Vec<u8>)> {
    let fw = pkt.width as usize;
    let (rx, ry, rw, rh) = clamp_roi(roi, fw, pkt.height as usize, min)?;
    let mut out = Vec::with_capacity(rw * rh * 4);
    for y in ry..ry + rh {
        let start = (y * fw + rx) * 4;
        for px in pkt.bgra[start..start + rw * 4].chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }
    Some((rw, rh, out))
}

/// 最近鄰縮小 + BGRA→RGBA,回傳 (w, h, rgba)。
fn make_preview(pkt: &FramePacket) -> (usize, usize, Vec<u8>) {
    let (w, h) = (pkt.width as usize, pkt.height as usize);
    let step = w.div_ceil(PREVIEW_MAX_W).max(1);
    let (pw, ph) = (w / step, h / step);
    let mut rgba = Vec::with_capacity(pw * ph * 4);
    for y in 0..ph {
        for x in 0..pw {
            let o = (y * step * w + x * step) * 4;
            rgba.extend_from_slice(&[pkt.bgra[o + 2], pkt.bgra[o + 1], pkt.bgra[o], 255]);
        }
    }
    (pw, ph, rgba)
}
