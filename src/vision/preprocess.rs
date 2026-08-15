//! OCR 前處理:遮罩 → 裁切文字實際範圍 → 黑字白底 → 放大 → 輕度模糊 → 白邊。
//! 半透明聊天背景後的遊戲畫面會被遮罩排除,OCR 只看到乾淨文字。

use crate::vision::classify::Palette;

const PAD: usize = 8;

/// 對一個 band 產生 OCR 輸入影像(BGRA)。
/// dom_entry: 主頻道在 palette.entries 的索引;None 時用亮度遮罩(未分類行)。
///
/// 只裁切、放大遮罩命中像素的實際左右範圍,而不是整個 ROI 寬度——
/// 一個 band 常常大部分是空白背景,整寬放大會讓圖片不必要地巨大,
/// 甚至可能超過 Windows OCR 的影像尺寸上限導致整行辨識失敗。
pub fn prepare_for_ocr(
    w: usize,
    bgra: &[u8],
    y0: usize,
    y1: usize,
    palette: &Palette,
    dom_entry: Option<usize>,
    scale: usize,
) -> (u32, u32, Vec<u8>) {
    let bh = y1 - y0;

    // 1) 建文字遮罩(全寬掃描以正確找出文字左右邊界):寬鬆容差(1.4x)抓回反鋸齒邊緣;
    //    未分類行退回亮度門檻。
    let mut mask = vec![false; w * bh];
    let mut min_x: Option<usize> = None;
    let mut max_x = 0usize;
    for yy in 0..bh {
        for x in 0..w {
            let o = ((y0 + yy) * w + x) * 4;
            let (b, g, r) = (bgra[o], bgra[o + 1], bgra[o + 2]);
            let hit = match dom_entry {
                Some(e) => {
                    let (_, c, tol) = palette.entries[e];
                    let t = (tol as f32 * 1.4) as i32;
                    (r as i32 - c[0] as i32).abs() <= t
                        && (g as i32 - c[1] as i32).abs() <= t
                        && (b as i32 - c[2] as i32).abs() <= t
                }
                None => r.max(g).max(b) >= 170,
            };
            if hit {
                mask[yy * w + x] = true;
                min_x = Some(min_x.map_or(x, |m| m.min(x)));
                max_x = max_x.max(x);
            }
        }
    }

    // 完全沒有命中像素:回傳一張最小的空白圖,讓呼叫端自然視為辨識不到文字。
    let Some(min_x) = min_x else {
        return (1, 1, vec![255u8; 4]);
    };

    // 左右各留一點邊界,避免緊貼裁切邊緣切掉筆畫。
    let pad_x = 4usize.min(w);
    let cx0 = min_x.saturating_sub(pad_x);
    let cx1 = (max_x + pad_x + 1).min(w);
    let cw = cx1 - cx0;

    // 2) 裁切出遮罩的實際範圍(不再額外膨脹——膨脹是在放大「前」的原始小尺寸上進行,
    //    1px 膨脹會被放大倍率等比例放大,CJK 筆畫間距本來就窄,很容易被放大後的膨脹
    //    直接黏成一塊,反而讓 OCR 更難辨識。遮罩本身的 1.4x 寬鬆容差已經足夠抓住
    //    反鋸齒邊緣,不需要再額外加粗)。
    let mut cropped = vec![false; cw * bh];
    for yy in 0..bh {
        for cx in 0..cw {
            cropped[yy * cw + cx] = mask[yy * w + cx0 + cx];
        }
    }

    // 3) 黑字白底灰階圖 + 最近鄰放大(僅裁切後的寬度)。
    let sw = cw * scale;
    let sh = bh * scale;
    let mut gray = vec![255u8; sw * sh];
    for yy in 0..sh {
        let sy = yy / scale;
        for x in 0..sw {
            if cropped[sy * cw + x / scale] {
                gray[yy * sw + x] = 0;
            }
        }
    }

    // 3b) 放大「後」再膨脹 1px,補回反鋸齒缺角造成的細筆畫斷裂。
    // 在放大後的尺寸上做,1px 就是真的 1px,不會像放大前膨脹那樣被等比例
    // 放大成一大塊、把相鄰筆畫/字黏在一起。
    let mut dilated = gray.clone();
    for yy in 0..sh {
        for x in 0..sw {
            if gray[yy * sw + x] != 0 {
                continue;
            }
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let ny = yy as i32 + dy;
                    let nx = x as i32 + dx;
                    if ny >= 0 && (ny as usize) < sh && nx >= 0 && (nx as usize) < sw {
                        dilated[ny as usize * sw + nx as usize] = 0;
                    }
                }
            }
        }
    }

    // 4) 3x3 均值模糊,柔化鋸齒讓 OCR 更穩。
    let mut blurred = dilated.clone();
    for yy in 1..sh.saturating_sub(1) {
        for x in 1..sw.saturating_sub(1) {
            let mut sum = 0u32;
            for dy in 0..3 {
                for dx in 0..3 {
                    sum += dilated[(yy + dy - 1) * sw + (x + dx - 1)] as u32;
                }
            }
            blurred[yy * sw + x] = (sum / 9) as u8;
        }
    }

    // 5) 加白邊並輸出 BGRA。
    let ow = sw + PAD * 2;
    let oh = sh + PAD * 2;
    let mut out = vec![255u8; ow * oh * 4];
    for yy in 0..sh {
        for x in 0..sw {
            let v = blurred[yy * sw + x];
            let o = ((yy + PAD) * ow + x + PAD) * 4;
            out[o] = v;
            out[o + 1] = v;
            out[o + 2] = v;
            // alpha 已是 255
        }
    }
    (ow as u32, oh as u32, out)
}
