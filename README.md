# 蘑菇聊天小幫手 (mushroom-chat-helper)

<img src="assets/icon-256.png" width="96" height="96" alt="蘑菇聊天小幫手圖示" />

[![CI](https://github.com/DellLin/mushroom-chat-helper/actions/workflows/ci.yml/badge.svg)](https://github.com/DellLin/mushroom-chat-helper/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/DellLin/mushroom-chat-helper?display_name=tag)](https://github.com/DellLin/mushroom-chat-helper/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

有鑑於新楓之谷：經典版的聊天室窗並沒有頻道分類的功能，部分玩家需要社交能量來當坐牢的精神糧食，而大量的廣播會把訊息快速洗掉。
此工具主要就是把把遊戲聊天視窗裡「你在乎的頻道」單獨拉出來,顯示成一個可以疊在遊戲上的獨立小視窗，讓你不會遺漏訊息。

判斷頻道的方式是**文字顏色**,抓到訊息後直接把擷取到的
畫面截圖顯示出來,所以看到的字跟遊戲裡長得一模一樣,不會有誤字。

**完全唯讀**——只擷取畫面,不讀寫遊戲記憶體、不注入、不模擬任何操作。

例:建立一個「社交」檢視 = 密語 + 好友 + 公會,獨立視窗就只顯示這三種對話,
再開啟置頂、把視窗貼齊遊戲原本的聊天視窗,就是一個濾過的聊天覆蓋層。

## 系統需求

- Windows 10 1903 以上

## 下載

到 [Releases](https://github.com/DellLin/mushroom-chat-helper/releases/latest) 下載
`mushroom-chat-helper-vX.Y.Z-windows-x64.zip`,解壓縮後直接執行裡面的 exe,不用安裝。
只想要執行檔的話也可以單獨下載 `mushroom-chat-helper.exe`(附 `.sha256` 可驗證)。

裝好之後不用再回來這裡:程式會自己檢查新版本,見 [USAGE.md](USAGE.md) 的「自動更新」。

## 快速上手

1. 開好遊戲、展開聊天視窗,啟動本程式
2. 按 **⚙** 打開設定 →「擷取設定」→ 選擇遊戲視窗 → **▶ 開始擷取**
3. 設定擷取範圍(ROI),框住聊天視窗最下面那一行
4. 到「頻道」分頁,依照需求建立或刪除頻道,並用「吸取畫面顏色」校正每個頻道的顏色
5. 到「檢視管理」建立自訂分頁(例如「社交」= 密語 + 好友 + 公會)

完整步驟、介面配置、快捷鍵、主畫面判斷、設定檔欄位等詳細說明,請見
**[USAGE.md](USAGE.md)**。

## 自行建置與開發

```bash
cargo build --release              # 產出 target/release/mushroom-chat-helper.exe
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

架構、資料流與各項設計取捨請見 [ARCHITECTURE.md](ARCHITECTURE.md)。
分支模型、commit 慣例與發版流程請見 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 授權與免責

MIT License,詳見 [LICENSE](LICENSE)。

僅供個人輔助使用。本工具不修改遊戲、不讀寫遊戲記憶體、不注入、不模擬操作;
使用任何第三方工具後果請自行負責。

## 贊助

如果這個工具幫到你,歡迎透過 [Portaly](https://portaly.cc/delllin) 請我喝杯飲料 ☕
