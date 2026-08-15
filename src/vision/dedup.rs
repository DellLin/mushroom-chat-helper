//! 兩層去重:文字遮罩雜湊(pre-OCR、省算力)與 OCR 後文字時間窗。

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

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
        if self.set.contains(&h) {
            return false;
        }
        self.set.insert(h);
        self.order.push_back(h);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

/// OCR 後文字去重:相似文字在時間窗內只放行一次。
///
/// 用模糊比對(編輯距離)而非精確字串比對——同一行文字在半透明背景抖動下,
/// 每次重跑 OCR 讀出來的雜訊都不太一樣(例如「謝謝你」「謝謝你胸」「謝謝作」),
/// 精確比對完全抓不到這種近似重複,時間窗拉再長也沒用。
pub struct TextDedup {
    recent: VecDeque<(String, Instant)>,
    window: Duration,
}

impl TextDedup {
    pub fn new(window_secs: u64) -> Self {
        Self { recent: VecDeque::new(), window: Duration::from_secs(window_secs.max(1)) }
    }

    pub fn set_window(&mut self, secs: u64) {
        self.window = Duration::from_secs(secs.max(1));
    }

    /// 新文字(跟時間窗內任何近似文字都不像)回傳 true。
    pub fn check_and_insert(&mut self, text: &str) -> bool {
        let now = Instant::now();
        let window = self.window;
        self.recent.retain(|(_, t)| now.duration_since(*t) < window);

        if self.recent.iter().any(|(s, _)| similar(s, text)) {
            return false;
        }
        self.recent.push_back((text.to_string(), now));
        while self.recent.len() > 400 {
            self.recent.pop_front();
        }
        true
    }
}

/// 過短字串差一兩個字元佔比就很大,容易誤判,設下限只對夠長的字串做模糊比對。
const SIMILAR_MIN_LEN: usize = 4;
/// 編輯距離佔較長字串長度的比例在此門檻以內視為同一則訊息(OCR 雜訊造成的差異)。
const SIMILAR_RATIO: f32 = 0.35;

fn similar(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let la = a.chars().count();
    let lb = b.chars().count();
    let max_len = la.max(lb);
    if la < SIMILAR_MIN_LEN || lb < SIMILAR_MIN_LEN {
        return false;
    }
    let dist = levenshtein(a, b);
    (dist as f32) <= (max_len as f32) * SIMILAR_RATIO
}

/// 逐字元(非 byte)編輯距離,適用中文。
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (la, lb) = (a.len(), b.len());
    let mut dp: Vec<usize> = (0..=lb).collect();
    for i in 1..=la {
        let mut prev = dp[0];
        dp[0] = i;
        for j in 1..=lb {
            let temp = dp[j];
            dp[j] = if a[i - 1] == b[j - 1] {
                prev
            } else {
                1 + prev.min(dp[j]).min(dp[j - 1])
            };
            prev = temp;
        }
    }
    dp[lb]
}

/// 同位置行的 OCR 冷卻:半透明背景造成遮罩微幅抖動時,避免同一行反覆重跑。
pub struct BandCooldown {
    map: HashMap<(u32, u8), Instant>,
    cooldown: Duration,
}

impl BandCooldown {
    pub fn new(ms: u64) -> Self {
        Self { map: HashMap::new(), cooldown: Duration::from_millis(ms) }
    }

    /// key: (y 位置桶, 頻道標籤)。可執行回傳 true 並記錄時間。
    pub fn allow(&mut self, y: usize, label: u8) -> bool {
        let key = ((y / 6) as u32, label);
        let now = Instant::now();
        if let Some(t) = self.map.get(&key) {
            if now.duration_since(*t) < self.cooldown {
                return false;
            }
        }
        self.map.insert(key, now);
        if self.map.len() > 512 {
            self.map.clear();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_catches_ocr_noise_variants() {
        let mut d = TextDedup::new(60);
        assert!(d.check_and_insert("羽薇: 這樣我就不突兀了"));
        // OCR 雜訊造成的近似變體應該被視為重複。
        assert!(!d.check_and_insert("羽薇: 這樣我就不突兀"));
        assert!(!d.check_and_insert("|羽薇: 這樣我就不突兀了羽微·?"));
    }

    #[test]
    fn dedup_allows_distinct_messages() {
        let mut d = TextDedup::new(60);
        assert!(d.check_and_insert("羽薇: 這樣我就不突兀了"));
        assert!(d.check_and_insert("一號: 我很為你著想"));
    }

    #[test]
    fn dedup_short_strings_need_exact_match() {
        let mut d = TextDedup::new(60);
        assert!(d.check_and_insert("羽:"));
        // 太短的字串不做模糊比對,避免不同短訊息被誤判成重複。
        assert!(d.check_and_insert("羽薇:"));
    }
}
