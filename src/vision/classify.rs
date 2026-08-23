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

/// 用分類結果建出一張二值遮罩,只保留 `target` 這個頻道文字顏色命中的像素
/// (其餘一律當背景剔除),打包成 bit 陣列供逐像素比對用。完全不看背景像素——
/// 半透明聊天背景後方的遊戲畫面移動再劇烈,只要文字本身沒變,遮罩就不會變。
///
/// 只保留單一頻道(而不是「任何頻道都算命中」),是因為同一個 ROI 位置偶爾會
/// 殘留別的頻道的雜色,那些跟這則訊息無關的像素會稀釋掉真正的比對訊號。
///
/// 刻意保留完整的 2D 形狀(不把整個 ROI 高度壓成一維水平投影),因為短訊息、
/// 或字數相同的連續訊息,用密度投影區分不出內容差異(不同文字的筆劃形狀差很
/// 多,但每欄的命中比例可能剛好相近)。ROI 固定框住同一個螢幕位置,同一畫面
/// 重複擷取時垂直對齊本來就很穩定,不需要犧牲 y 軸解析度來換取容錯。
pub fn channel_mask_bits(labels: &[u8], target: u8) -> Vec<u64> {
    let mut bits = vec![0u64; labels.len().div_ceil(64)];
    for (i, &l) in labels.iter().enumerate() {
        if l == target {
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

    /// 在 w x h 的標記陣列裡,把 [x0,x1) 這段欄位全標成命中指定頻道,其餘保持 NO_LABEL。
    fn labels_with_hit_columns(w: usize, h: usize, x0: usize, x1: usize, label: u8) -> Vec<u8> {
        let mut labels = vec![NO_LABEL; w * h];
        for y in 0..h {
            for x in x0..x1 {
                labels[y * w + x] = label;
            }
        }
        labels
    }

    #[test]
    fn mask_similarity_low_for_same_length_different_content() {
        let (w, h) = (100, 20);
        // 兩則訊息命中的面積(等同「字數」)完全相同,但落在完全不同的位置——
        // 模擬「字數相同但內容不同」的連續短訊息,保留完整 2D 形狀後相似度應該很低。
        let a = channel_mask_bits(&labels_with_hit_columns(w, h, 0, 20, 0), 0);
        let b = channel_mask_bits(&labels_with_hit_columns(w, h, 80, 100, 0), 0);
        assert!(mask_similarity(&a, &b) < 0.1);
    }

    #[test]
    fn mask_similarity_high_for_minor_jitter() {
        let (w, h) = (100, 20);
        let a = channel_mask_bits(&labels_with_hit_columns(w, h, 10, 30, 0), 0);
        // 反鋸齒抖動的模擬:邊界只差 1 欄。
        let b = channel_mask_bits(&labels_with_hit_columns(w, h, 10, 29, 0), 0);
        assert!(mask_similarity(&a, &b) > 0.9);
    }

    #[test]
    fn channel_mask_bits_only_keeps_target_channel() {
        let (w, h) = (40, 10);
        let mut labels = vec![NO_LABEL; w * h];
        for y in 0..h {
            for x in 0..20 {
                labels[y * w + x] = 0; // 頻道 0
            }
            for x in 20..40 {
                labels[y * w + x] = 1; // 頻道 1
            }
        }
        let popcount = |v: &[u64]| v.iter().map(|x| x.count_ones()).sum::<u32>();
        assert_eq!(popcount(&channel_mask_bits(&labels, 0)), (w * h / 2) as u32);
        assert_eq!(popcount(&channel_mask_bits(&labels, 1)), (w * h / 2) as u32);
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
