# CLAUDE.md

給未來的 Claude Code session(以及任何接手的人)看的專案操作規則。
專案本身的功能說明看 [README.md](README.md),架構與設計取捨看 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 專案速覽

Windows 桌面應用,Rust + egui/eframe。擷取遊戲畫面 → 依文字顏色分類頻道 →
直接顯示對話截圖(**不做 OCR**)。相依 `Windows.Graphics.Capture` 與 Win32 API,
**只能在 Windows 上建置與執行**。

## 常用指令

```bash
cargo build --release      # 產出 target/release/mushroom-chat-helper.exe
cargo test --all
cargo clippy --all-targets --all-features -- -D warnings
```

`.cargo/config.toml` 把 target-dir 指到 `C:/rust-target/...` 以繞過 Windows
MAX_PATH 限制,這是**本機開發環境專屬**設定,已在 `.gitignore` 排除,不要提交。

## Git workflow(重要:要推去哪個分支)

```
feat/*  fix/*  docs/*  refactor/*   ← 實際開發在這裡,從 develop 開出來
        │  PR
        ▼
      develop                        ← 日常整合分支,「預設推送目標」
        │  PR(功能穩定、準備發版時)
        ▼
       main                          ← 永遠保持可發佈狀態
        │  打 tag vX.Y.Z
        ▼
   GitHub Release(自動建置 .exe)
```

**預設推送目標是 `develop`,不是 `main`。**

- 一般調整/新功能:從 `develop` 開 `feat/簡短描述` 分支 → 推上去 → 開 PR 回 `develop`。
- 很小的修正(改錯字、調註解)可以直接推 `develop`。
- **不要直接推 `main`**,`main` 只透過 PR 從 `develop` 合併進來。
- 不要自作主張打 tag 或發 Release,那等於對外發版 —— 先問過使用者。

### Commit message

Conventional Commits,主旨用繁體中文(跟既有歷史一致):

```
feat: 新增 XXX 功能
fix(vision): 修正顏色容差在深色頻道下的誤判
docs: 更新 README 的設定說明
chore(config): 調整預設值
refactor(capture): 精簡 D3D11 裝置建立
```

## CI / CD

| Workflow | 檔案 | 觸發時機 | 做什麼 |
|---|---|---|---|
| CI | [.github/workflows/ci.yml](.github/workflows/ci.yml) | push / PR 到 `main`、`develop` | clippy(`-D warnings`)、`cargo test`、release build,並上傳 exe artifact(留 14 天) |
| Release | [.github/workflows/release.yml](.github/workflows/release.yml) | push tag `v*.*.*`(或手動 dispatch) | 檢查版號、跑完整檢查、打包 zip + SHA256、建立 GitHub Release |

兩個 workflow 都跑在 `windows-latest`(唯一能建置本專案的 runner)。

### 發版(部署)流程

1. 確認 `develop` 的 CI 是綠的,PR 合併進 `main`。
2. 更新 `Cargo.toml` 的 `version`(release workflow 會檢查 tag 與它是否一致,
   不一致直接失敗)。
3. 從 `main` 打 tag 並推上去:

   ```bash
   git checkout main && git pull
   git tag v0.2.0 && git push origin v0.2.0
   ```

4. Release workflow 自動建置並發佈,產出 `mushroom-chat-helper-v0.2.0-windows-x64.zip`
   與 `.sha256`。

版號依 SemVer:破壞性變更改 MAJOR、新功能改 MINOR、修 bug 改 PATCH。

## 注意事項

- 這個 repo 是 **public**,提交前確認沒有帶入個人路徑、憑證或個人資訊。
- `mushroom_chat_helper.toml`(執行期產生的設定檔)與 `.cargo/` 已被 gitignore,
  不要提交 —— 裡面是使用者自己校正的座標與顏色。
- CI 目前**沒有** `cargo fmt --check`,因為既有程式碼有刻意的手寫排版。
  若要加上,得先跑一次 `cargo fmt --all` 全面重排。
