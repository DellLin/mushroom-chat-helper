//! 切行:固定高度網格,不依賴像素內容判斷行界。

#[derive(Clone, Copy, Debug)]
pub struct Band {
    /// [y0, y1)
    pub y0: usize,
    pub y1: usize,
}

/// 從 ROI 最上面開始,依固定高度切出連續行,每行都視為一則候選訊息。
/// 不看該範圍是否真的有文字像素——是否成立一則訊息由後續分類/OCR 決定。
/// 最後不足一整行高度的餘數會被捨棄。
pub fn fixed_bands(total_h: usize, row_h: usize) -> Vec<Band> {
    if row_h == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut y0 = 0usize;
    while y0 + row_h <= total_h {
        out.push(Band { y0, y1: y0 + row_h });
        y0 += row_h;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_grid() {
        let bands = fixed_bands(100, 25);
        assert_eq!(bands.len(), 4);
        assert_eq!(bands[0].y0, 0);
        assert_eq!(bands[0].y1, 25);
        assert_eq!(bands[3].y0, 75);
        assert_eq!(bands[3].y1, 100);
    }

    #[test]
    fn remainder_dropped() {
        let bands = fixed_bands(60, 25);
        assert_eq!(bands.len(), 2);
        assert_eq!(bands[1].y1, 50);
    }
}
