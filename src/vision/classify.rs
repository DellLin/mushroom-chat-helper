//! 像素 → 頻道分類。以「文字顏色」比對頻道色票。

use crate::config::ChannelDef;

pub const NO_LABEL: u8 = 255;

/// 標記階段的容差倍率:只認文字實心的「核心」部分,避免半透明聊天背景後方的
/// 遊戲畫面落進容差範圍造成誤判,連帶讓遮罩雜湊不穩定。
const CORE_TOL_MUL: f32 = 0.55;

/// 主頻道判定門檻:命中最多的頻道要佔全部命中像素的這個比例以上才算數,
/// 否則視為未分類(多個頻道顏色混在一起,分不出這行到底屬於誰)。
const DOMINANT_RATIO: f32 = 0.45;

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

    /// 回傳最接近且落在核心容差內的色票索引(entries 的索引)。
    #[inline]
    fn match_px(&self, r: u8, g: u8, b: u8) -> Option<usize> {
        // 聊天文字皆為亮色,先用亮度快篩省掉大多數背景像素。
        if r.max(g).max(b) < self.floor {
            return None;
        }
        let mut best: Option<(usize, u32)> = None;
        for (i, (_, c, tol)) in self.entries.iter().enumerate() {
            let t = (*tol as f32 * CORE_TOL_MUL) as i32;
            let dr = r as i32 - c[0] as i32;
            let dg = g as i32 - c[1] as i32;
            let db = b as i32 - c[2] as i32;
            if dr.abs() <= t && dg.abs() <= t && db.abs() <= t {
                let d = (dr * dr + dg * dg + db * db) as u32;
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
        }
        best.map(|(i, _)| i)
    }
}

/// 對 ROI 每個像素標記所屬頻道(NO_LABEL = 沒有任何頻道命中)。
pub fn label_pixels(rgba: &[u8], palette: &Palette) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .map(|px| match palette.match_px(px[0], px[1], px[2]) {
            Some(idx) => idx as u8,
            None => NO_LABEL,
        })
        .collect()
}

pub struct LabelStats {
    /// palette entries 索引 → 命中像素數
    pub counts: Vec<u32>,
    pub total: u32,
}

impl LabelStats {
    pub fn new(labels: &[u8], palette_len: usize) -> Self {
        let mut counts = vec![0u32; palette_len];
        let mut total = 0u32;
        for &l in labels {
            if l != NO_LABEL {
                counts[l as usize] += 1;
                total += 1;
            }
        }
        Self { counts, total }
    }

    /// 主頻道:需佔命中像素的 DOMINANT_RATIO 以上且至少 min_px。
    pub fn dominant(&self, min_px: u32) -> Option<usize> {
        let (best_i, best_c) = self
            .counts
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| **c)
            .map(|(i, c)| (i, *c))?;
        if best_c >= min_px
            && self.total > 0
            && (best_c as f32 / self.total as f32) >= DOMINANT_RATIO
        {
            Some(best_i)
        } else {
            None
        }
    }
}

/// 去重用的遮罩:直接拿原始像素跟「已經判定出來的單一頻道」顏色比對,打包成
/// bit 陣列供逐像素比對用,完全不看背景像素——半透明聊天背景後方的遊戲畫面
/// 移動再劇烈,只要文字本身沒變,遮罩就不會變。
///
/// 刻意用完整容差(`tolerance`),不是 `Palette::match_px` 分類時縮小過的核心
/// 容差:核心容差縮小是為了在多個頻道顏色相近時分辨誰是誰(見 CORE_TOL_MUL
/// 說明),但這裡已經由呼叫端決定好是哪個頻道了,不需要再跟其他頻道的顏色
/// 比賽。半透明聊天背景會讓同一顆字的顏色隨背後畫面(玩家移動、背景特效)
/// 小幅偏移,核心容差往往窄到連文字實心的部分都會被推出範圍,導致同一則
/// 訊息在畫面間被誤判成內容變了、反覆重新顯示;完整容差才禁得起這種偏移。
///
/// 刻意保留完整的 2D 形狀(不把整個 ROI 高度壓成一維水平投影),因為短訊息、
/// 或字數相同的連續訊息,用密度投影區分不出內容差異(不同文字的筆劃形狀差很
/// 多,但每欄的命中比例可能剛好相近)。ROI 固定框住同一個螢幕位置,同一畫面
/// 重複擷取時垂直對齊本來就很穩定,不需要犧牲 y 軸解析度來換取容錯。
pub fn dedup_mask_bits(rgba: &[u8], color: [u8; 3], tolerance: u8) -> Vec<u64> {
    let t = tolerance as i32;
    let chunks = rgba.as_chunks::<4>().0;
    let mut bits = vec![0u64; chunks.len().div_ceil(64)];
    for (i, px) in chunks.iter().enumerate() {
        let dr = px[0] as i32 - color[0] as i32;
        let dg = px[1] as i32 - color[1] as i32;
        let db = px[2] as i32 - color[2] as i32;
        if dr.abs() <= t && dg.abs() <= t && db.abs() <= t {
            bits[i / 64] |= 1u64 << (i % 64);
        }
    }
    bits
}

/// 兩張遮罩的相似度(Jaccard/IoU:交集 ÷ 聯集),1.0 表示完全相同、0.0 表示
/// 完全沒有重疊像素。用比例而非絕對差異像素數,是因為短訊息命中的像素本來
/// 就少,絕對差異值容易被大片背景稀釋掉,比例才能公平反映「內容像不像」,
/// 不論訊息長短。兩張都是空遮罩時視為相同(避免除以零)。
pub fn mask_similarity(a: &[u64], b: &[u64]) -> f32 {
    let mut inter = 0u32;
    let mut union = 0u32;
    for (x, y) in a.iter().zip(b.iter()) {
        inter += (x & y).count_ones();
        union += (x | y).count_ones();
    }
    if union == 0 {
        1.0
    } else {
        inter as f32 / union as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一個 n bit 的遮罩,把 [x0,x1) 這段 bit 標成命中,其餘為 0(只給下面
    /// mask_similarity 測試搭積木用,production code 已改直接對原始像素做
    /// 去重比對,見 dedup_mask_bits)。
    fn bits(n: usize, x0: usize, x1: usize) -> Vec<u64> {
        let mut v = vec![0u64; n.div_ceil(64)];
        for i in x0..x1 {
            v[i / 64] |= 1u64 << (i % 64);
        }
        v
    }

    #[test]
    fn mask_similarity_low_for_same_length_different_content() {
        // 兩則訊息命中的 bit 數(等同「字數」)完全相同,但完全不重疊——
        // 模擬「字數相同但內容不同」的連續短訊息,相似度應該很低。
        let a = bits(2000, 0, 20);
        let b = bits(2000, 1600, 1620);
        assert!(mask_similarity(&a, &b) < 0.1);
    }

    #[test]
    fn mask_similarity_high_for_minor_jitter() {
        let a = bits(2000, 10, 30);
        // 反鋸齒抖動的模擬:邊界只差 1 個 bit。
        let b = bits(2000, 10, 29);
        assert!(mask_similarity(&a, &b) > 0.9);
    }

    #[test]
    fn dedup_mask_only_keeps_pixels_within_full_tolerance() {
        let color = [255u8, 255, 255];
        #[rustfmt::skip]
        let rgba = [
            250, 250, 250, 255, // 容差 20 以內 → 命中
            180, 180, 180, 255, // 超出容差 → 不命中
        ];
        let mask = dedup_mask_bits(&rgba, color, 20);
        assert_eq!(mask[0] & 0b11, 0b01);
    }

    /// 這是這個函式存在的理由:半透明聊天背景造成的顏色偏移常常超過
    /// `Palette::match_px` 用的核心容差(CORE_TOL_MUL 縮小過),但去重不需要
    /// 分辨頻道,用完整容差仍然要能命中,遮罩才不會因為背景變動而跳動。
    #[test]
    fn dedup_mask_survives_a_shift_beyond_core_tolerance() {
        let color = [255u8, 255, 255];
        let tol = 20u8;
        let core_tol = (tol as f32 * CORE_TOL_MUL) as i32; // 11
        let shifted = 255 - (core_tol + 3); // 超出核心容差,但仍在完整容差內
        let rgba = [shifted as u8, shifted as u8, shifted as u8, 255];
        let mask = dedup_mask_bits(&rgba, color, tol);
        assert_eq!(mask[0] & 1, 1);
    }

    #[test]
    fn dominant_needs_a_clear_majority() {
        // 單一頻道佔滿 → 明確分類。
        let mut solo = vec![NO_LABEL; 100];
        solo[..60].fill(0);
        assert_eq!(LabelStats::new(&solo, 2).dominant(20), Some(0));

        // 三個頻道混在一起,最高的只佔 40% → 低於門檻,視為未分類。
        let mut mixed = vec![NO_LABEL; 100];
        mixed[..40].fill(0);
        mixed[40..70].fill(1);
        mixed[70..].fill(2);
        assert_eq!(LabelStats::new(&mixed, 3).dominant(20), None);

        // 命中像素太少(雜訊)→ 即使比例夠高也不算數。
        let mut sparse = vec![NO_LABEL; 100];
        sparse[..5].fill(0);
        assert_eq!(LabelStats::new(&sparse, 2).dominant(20), None);
    }
}
