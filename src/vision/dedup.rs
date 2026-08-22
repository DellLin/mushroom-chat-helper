//! 去重/節流:標記雜湊(避免重複處理未變動的畫面)、同頻道冷卻,
//! 以及畫面去重(避免同一則訊息因背景抖動重複顯示)。

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use super::classify::mask_similarity;

/// 固定容量的雜湊 LRU 集合。
pub struct LruSet {
    set: HashSet<u64>,
    order: VecDeque<u64>,
    cap: usize,
}

impl LruSet {
    pub fn new(cap: usize) -> Self {
        Self { set: HashSet::new(), order: VecDeque::new(), cap }
    }

    /// 新雜湊回傳 true 並記錄;已見過回傳 false。
    pub fn insert_if_new(&mut self, h: u64) -> bool {
        if !self.set.insert(h) {
            return false;
        }
        self.order.push_back(h);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

/// 同頻道冷卻:半透明背景造成標記微幅抖動時,避免同一則訊息在短時間內
/// 被反覆送出。鍵是頻道標籤,所以不同頻道的訊息不會互相擋住。
pub struct ChannelCooldown {
    last: HashMap<u8, Instant>,
    cooldown: Duration,
}

impl ChannelCooldown {
    pub fn new(ms: u64) -> Self {
        Self { last: HashMap::new(), cooldown: Duration::from_millis(ms) }
    }

    /// 該頻道已過冷卻時間回傳 true 並記錄時間。
    pub fn allow(&mut self, label: u8) -> bool {
        let now = Instant::now();
        if let Some(t) = self.last.get(&label) {
            if now.duration_since(*t) < self.cooldown {
                return false;
            }
        }
        self.last.insert(label, now);
        true
    }
}

/// 兩張遮罩的相似度(見 classify::mask_similarity)在此門檻以上視為同一則訊息。
/// 同一則訊息只有反鋸齒邊緣的微幅抖動,交集/聯集比例通常很高(>0.9);
/// 不同內容(即使字數、命中面積相近)的訊息因為實際筆劃形狀不同,重疊比例
/// 通常遠低於此。
const SIMILARITY_THRESHOLD: f32 = 0.55;

/// 畫面去重:新畫面只跟「前一筆」畫面比對是不是同一則訊息(見
/// classify::channel_mask_bits),不維護一段時間窗內的多筆歷史紀錄。
///
/// ROI 只框住單一則對話,每次擷取範圍裡最多只有一則訊息,只要問「這一次擷取
/// 到的畫面,跟上一次是不是同一則」就夠了。遮罩只保留該訊息所屬頻道的文字
/// 顏色命中像素、不含背景像素,所以玩家移動、背景特效造成的畫面劇烈變動不會
/// 影響判斷,只有文字本身真的改變才會被視為新訊息。
pub struct ImageDedup {
    last: Option<(Vec<u64>, Instant)>,
    window: Duration,
}

impl ImageDedup {
    pub fn new(window_secs: u64) -> Self {
        Self { last: None, window: Duration::from_secs(window_secs.max(1)) }
    }

    pub fn set_window(&mut self, secs: u64) {
        self.window = Duration::from_secs(secs.max(1));
    }

    /// 新遮罩(跟前一筆不像,或前一筆已經超過時間窗)回傳 true 並記錄。
    pub fn check_and_insert(&mut self, mask: Vec<u64>) -> bool {
        let now = Instant::now();
        let is_repeat = self.last.as_ref().is_some_and(|(prev, t)| {
            now.duration_since(*t) < self.window
                && mask_similarity(prev, &mask) >= SIMILARITY_THRESHOLD
        });
        self.last = Some((mask, now));
        !is_repeat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一個 n bit 的遮罩,把 [x0,x1) 這段 bit 標成命中,其餘為 0。
    fn bits(n: usize, x0: usize, x1: usize) -> Vec<u64> {
        let mut v = vec![0u64; n.div_ceil(64)];
        for i in x0..x1 {
            v[i / 64] |= 1u64 << (i % 64);
        }
        v
    }

    #[test]
    fn lru_set_forgets_oldest_beyond_capacity() {
        let mut lru = LruSet::new(2);
        assert!(lru.insert_if_new(1));
        assert!(lru.insert_if_new(2));
        assert!(!lru.insert_if_new(1));
        assert!(lru.insert_if_new(3)); // 塞入 3 之後 1 被擠出
        assert!(lru.insert_if_new(1));
    }

    #[test]
    fn channel_cooldown_is_per_channel() {
        let mut cd = ChannelCooldown::new(60_000);
        assert!(cd.allow(0));
        assert!(!cd.allow(0));
        // 另一個頻道不受影響。
        assert!(cd.allow(1));
    }

    #[test]
    fn image_dedup_catches_near_identical_masks() {
        let mut d = ImageDedup::new(60);
        let a = bits(200, 10, 50);
        let b = bits(200, 10, 49); // 反鋸齒抖動造成的微幅差異(邊界少 1 bit)
        assert!(d.check_and_insert(a));
        assert!(!d.check_and_insert(b));
    }

    #[test]
    fn image_dedup_allows_distinct_consecutive_messages() {
        let mut d = ImageDedup::new(60);
        // 兩則訊息命中的 bit 數(等同字數)相同,但完全不重疊——
        // 模擬「字數相同但內容不同」的連續訊息。
        let a = bits(200, 10, 30);
        let b = bits(200, 60, 80);
        assert!(d.check_and_insert(a));
        assert!(d.check_and_insert(b));
    }

    #[test]
    fn image_dedup_catches_a_after_returning_from_b() {
        let mut d = ImageDedup::new(60);
        let a = bits(200, 10, 30);
        let b = bits(200, 60, 80);
        assert!(d.check_and_insert(a.clone()));
        assert!(d.check_and_insert(b));
        // 只跟「前一筆」比,回到 a 這種內容因為前一筆是 b,會被視為新訊息。
        assert!(d.check_and_insert(a));
    }

    #[test]
    fn image_dedup_expires_after_window() {
        let mut d = ImageDedup::new(0);
        let a = bits(200, 10, 30);
        assert!(d.check_and_insert(a.clone()));
        std::thread::sleep(Duration::from_millis(1100));
        // 時間窗已過期,同一遮罩應再次被視為新訊息。
        assert!(d.check_and_insert(a));
    }
}
