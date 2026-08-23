//! 影像管線執行緒:ROI 裁切 → 像素標記 → 依文字顏色分類 → 把畫面送到 UI。
//! ROI 本身就是單一則對話的範圍,不做 OCR/文字辨識,聊天內容以擷取到的
//! 原始畫面顯示。

pub mod classify;
pub mod dedup;

use std::hash::Hasher;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

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

/// 超過這段時間沒收到新畫面,就當作主畫面判斷已經沒有依據可用(遊戲關掉、
/// 最小化、擷取被停掉或卡住),主動回報一次「在主畫面」。
///
/// 這是自動隱藏的安全網:沒有畫面就等不到下一次狀態改變,UI 那邊的聊天視窗
/// 會永遠留在螢幕外面叫不回來。畫面回來後第一幀會重新回報真正的判斷結果。
const FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(3);

/// 主畫面判斷要怎麼回報給 UI。
///
/// 把每幀的判斷結果原封不動送出去是不夠的:UI 收到 `false` 會把聊天視窗搬到
/// 螢幕外,而「從頭到尾都是 false」完全可能發生(判斷顏色沒校正好、換了解析度、
/// 遊戲改版換色),那樣視窗就永遠回不來。所以多押一個條件——至少真的看過一次
/// 「在主畫面」,之後的 `false` 才算數;還沒看過就一律回報 `true`(畫面照樣
/// 跳過不處理,只是不隱藏視窗)。設定被改過、或畫面斷過,都要重新確認。
#[derive(Default)]
struct GateReporter {
    /// 已經確認過「這組判斷設定認得出主畫面」。
    confirmed: bool,
    /// 上一次回報給 UI 的值。None = 還沒回報過,下一次一定送。
    reported: Option<bool>,
}

impl GateReporter {
    /// 吃一幀的判斷結果,回傳這次要送給 UI 的值(None = 跟上次一樣,不用送)。
    fn on_frame(&mut self, is_open: bool) -> Option<bool> {
        self.confirmed |= is_open;
        let report = is_open || !self.confirmed;
        if self.reported == Some(report) {
            return None;
        }
        self.reported = Some(report);
        Some(report)
    }

    /// 畫面斷流(遊戲關掉、最小化、擷取停掉或卡住):主畫面判斷已經沒有依據可用,
    /// 先把視窗放出來。同時忘掉確認與回報狀態——畫面回來時可能已經換了視窗或
    /// 解析度,得重新確認一次。
    fn on_idle(&mut self) -> Option<bool> {
        let need_show = self.reported == Some(false);
        self.reported = None;
        self.confirmed = false;
        need_show.then_some(true)
    }

    /// 判斷設定被改過(重新校正顏色/範圍),之前的確認不算數了。
    fn forget_confirmation(&mut self) {
        self.confirmed = false;
    }
}

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
    // 主畫面判斷回報給 UI 的節流與安全規則,見 GateReporter。
    let mut gate = GateReporter::default();
    // 上一幀的主畫面判斷設定,改過就要重新確認一次。
    let mut prev_gate_cfg: Option<GateRoi> = None;

    loop {
        let pkt = match frame_rx.recv_timeout(FRAME_IDLE_TIMEOUT) {
            Ok(pkt) => pkt,
            Err(RecvTimeoutError::Timeout) => {
                if let Some(open) = gate.on_idle() {
                    let _ = ui_tx.send(UiEvent::GateState(open));
                }
                continue;
            }
            // 擷取執行緒收攤了,管線也跟著結束。
            Err(RecvTimeoutError::Disconnected) => break,
        };

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

        if prev_gate_cfg.as_ref() != Some(&cfg_snap.gate) {
            prev_gate_cfg = Some(cfg_snap.gate.clone());
            gate.forget_confirmation();
        }
        if let Some(open) = gate.on_frame(is_gate_open) {
            let _ = ui_tx.send(UiEvent::GateState(open));
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
    for px in bgra.as_chunks::<4>().0 {
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
        for px in pkt.bgra[start..start + rw * 4].as_chunks::<4>().0 {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 判斷顏色沒校正好(從頭到尾都判定不在主畫面)時,絕對不能回報 false
    /// ——那會讓 UI 把聊天視窗搬到螢幕外再也回不來。
    #[test]
    fn never_reports_closed_before_seeing_an_open_frame() {
        let mut g = GateReporter::default();
        assert_eq!(g.on_frame(false), Some(true), "第一幀就要回報,而且是 true");
        for _ in 0..10 {
            assert_eq!(g.on_frame(false), None, "值沒變就不用重送");
        }
    }

    /// 確認過判斷有效之後,離開主畫面才算數。
    #[test]
    fn reports_closed_once_the_gate_has_proven_itself() {
        let mut g = GateReporter::default();
        assert_eq!(g.on_frame(true), Some(true));
        assert_eq!(g.on_frame(false), Some(false), "確認過了,這個 false 算數");
        assert_eq!(g.on_frame(false), None);
        assert_eq!(g.on_frame(true), Some(true), "回到主畫面要通知 UI 還原視窗");
    }

    /// 畫面斷流(擷取的遊戲關掉/最小化)一定要把視窗放出來。
    #[test]
    fn idle_pulls_the_window_back_out() {
        let mut g = GateReporter::default();
        g.on_frame(true);
        assert_eq!(g.on_frame(false), Some(false));
        assert_eq!(g.on_idle(), Some(true), "沒有畫面就沒有依據,先讓視窗出來");
        // 畫面回來後要重新確認一次,確認前不准再回報 false。
        assert_eq!(g.on_frame(false), Some(true));
        assert_eq!(g.on_frame(true), None, "上一次回報的就是 true,不用重送");
        assert_eq!(g.on_frame(false), Some(false));
    }

    /// 已經是「在主畫面」時斷流,不用多送一次 true。
    #[test]
    fn idle_says_nothing_when_the_window_is_already_out() {
        let mut g = GateReporter::default();
        assert_eq!(g.on_frame(true), Some(true));
        assert_eq!(g.on_idle(), None);
    }

    /// 重新校正判斷設定後,新設定要自己重新證明一次。
    #[test]
    fn recalibrating_the_gate_requires_a_new_confirmation() {
        let mut g = GateReporter::default();
        g.on_frame(true);
        g.forget_confirmation();
        assert_eq!(g.on_frame(false), None, "上次回報的就是 true,值沒變");
        assert_eq!(g.on_frame(false), None, "新設定還沒證明過,不准回報 false");
        assert_eq!(g.on_frame(true), None);
        assert_eq!(g.on_frame(false), Some(false), "證明過了才算數");
    }
}
