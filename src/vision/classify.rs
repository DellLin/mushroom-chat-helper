//! 像素 → 頻道分類。以「文字顏色」比對頻道色票。

use crate::config::ChannelDef;

pub const NO_LABEL: u8 = 255;

/// 標記階段用的核心容差倍率(見 label_pixels)。
const CORE_TOL_MUL: f32 = 0.55;

/// 由啟用中的頻道建立的色票。
pub struct Palette {
    /// (config 內索引, RGB, tolerance)
    pub entries: Vec<(usize, [u8; 3], u8)>,
    /// 亮度快篩門檻:由色票動態算出,確保設定了較暗顏色(如深色頻道字)
    /// 的頻道,不會被寫死的門檻擋在比對邏輯之前(容差滑桿救不了這種情況)。
    floor: u8,
}

impl Palette {
    pub fn from_channels(channels: &[ChannelDef]) -> Self {
        let entries: Vec<(usize, [u8; 3], u8)> = channels
            .iter()
            .enumerate()
            .filter(|(_, c)| c.enabled)
            .map(|(i, c)| (i, c.color, c.tolerance))
            .collect();
        let floor = entries
            .iter()
            .map(|(_, c, tol)| {
                let core_tol = (*tol as f32 * CORE_TOL_MUL) as i32;
                let brightest = c[0].max(c[1]).max(c[2]) as i32;
                (brightest - core_tol).clamp(0, 255) as u8
            })
            .min()
            .unwrap_or(90);
        Self { entries, floor }
    }

    /// 回傳最接近且落在容差內的色票索引(entries 的索引)。
    #[inline]
    pub fn match_px(&self, r: u8, g: u8, b: u8, tol_mul: f32) -> Option<usize> {
        // 聊天文字皆為亮色,先用亮度快篩省掉大多數背景像素。
        if r.max(g).max(b) < self.floor {
            return None;
        }
        let mut best: Option<(usize, u32)> = None;
        for (i, (_, c, tol)) in self.entries.iter().enumerate() {
            let t = ((*tol as f32) * tol_mul) as i32;
            let dr = r as i32 - c[0] as i32;
            let dg = g as i32 - c[1] as i32;
            let db = b as i32 - c[2] as i32;
            if dr.abs() <= t && dg.abs() <= t && db.abs() <= t {
                let d = (dr * dr + dg * dg + db * db) as u32;
                if best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
        }
        best.map(|(i, _)| i)
    }
}

/// 對 ROI 每個像素標記頻道(NO_LABEL=無),並回傳每列符合數。
/// 使用「核心容差」(tol_mul=0.55) 只抓文字實心部分,
/// 避免半透明聊天背景後的遊戲畫面造成誤判與雜湊不穩。
pub fn label_pixels(w: usize, h: usize, bgra: &[u8], palette: &Palette) -> (Vec<u8>, Vec<u32>) {
    let mut labels = vec![NO_LABEL; w * h];
    let mut rows = vec![0u32; h];
    for y in 0..h {
        let mut count = 0u32;
        for x in 0..w {
            let o = (y * w + x) * 4;
            let (b, g, r) = (bgra[o], bgra[o + 1], bgra[o + 2]);
            if let Some(idx) = palette.match_px(r, g, b, 0.55) {
                labels[y * w + x] = idx as u8;
                count += 1;
            }
        }
        rows[y] = count;
    }
    (labels, rows)
}

pub struct BandStats {
    /// palette entries 索引 → 像素數
    pub counts: Vec<u32>,
    pub total: u32,
}

impl BandStats {
    /// 主頻道:需佔符合像素的 45% 以上且至少 min_px。
    pub fn dominant(&self, min_px: u32) -> Option<usize> {
        let (best_i, best_c) = self
            .counts
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| **c)
            .map(|(i, c)| (i, *c))?;
        if best_c >= min_px && self.total > 0 && (best_c as f32 / self.total as f32) >= 0.45 {
            Some(best_i)
        } else {
            None
        }
    }
}

pub fn band_stats(
    w: usize,
    labels: &[u8],
    y0: usize,
    y1: usize,
    palette_len: usize,
) -> BandStats {
    let mut counts = vec![0u32; palette_len];
    let mut total = 0u32;
    for y in y0..y1 {
        for x in 0..w {
            let l = labels[y * w + x];
            if l != NO_LABEL {
                counts[l as usize] += 1;
                total += 1;
            }
        }
    }
    BandStats { counts, total }
}

/// 校準用:取 band 內「已被標記為指定頻道」的像素平均色。
/// 比 dominant_bright_color 準確,因為只看分類階段核心容差比對成功的像素,
/// 不會被面積較大的背景色蓋過去(背景通常比文字筆畫佔更多像素)。
pub fn labeled_avg_color(
    w: usize,
    bgra: &[u8],
    labels: &[u8],
    target: u8,
    y0: usize,
    y1: usize,
) -> Option<[u8; 3]> {
    let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u32);
    for y in y0..y1 {
        for x in 0..w {
            if labels[y * w + x] != target {
                continue;
            }
            let o = (y * w + x) * 4;
            let (b, g, r) = (bgra[o], bgra[o + 1], bgra[o + 2]);
            sr += r as u64;
            sg += g as u64;
            sb += b as u64;
            n += 1;
        }
    }
    if n < 4 {
        return None;
    }
    Some([(sr / n as u64) as u8, (sg / n as u64) as u8, (sb / n as u64) as u8])
}

/// 校準用(未分類行退回):取 band 內「亮像素」的量化眾數色(RGB/16 分桶後取最大桶平均)。
pub fn dominant_bright_color(
    w: usize,
    bgra: &[u8],
    y0: usize,
    y1: usize,
) -> Option<[u8; 3]> {
    use std::collections::HashMap;
    let mut buckets: HashMap<(u8, u8, u8), (u64, u64, u64, u32)> = HashMap::new();
    for y in y0..y1 {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let (b, g, r) = (bgra[o], bgra[o + 1], bgra[o + 2]);
            if r.max(g).max(b) < 140 {
                continue; // 只看亮像素(文字)
            }
            let key = (r >> 4, g >> 4, b >> 4);
            let e = buckets.entry(key).or_insert((0, 0, 0, 0));
            e.0 += r as u64;
            e.1 += g as u64;
            e.2 += b as u64;
            e.3 += 1;
        }
    }
    let (_, (sr, sg, sb, n)) = buckets.into_iter().max_by_key(|(_, v)| v.3)?;
    if n < 12 {
        return None;
    }
    Some([(sr / n as u64) as u8, (sg / n as u64) as u8, (sb / n as u64) as u8])
}
