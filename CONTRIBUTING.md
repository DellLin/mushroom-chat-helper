# 開發與貢獻

## 環境需求

- Windows 10 1903 以上(`Windows.Graphics.Capture` 的需求)
- [Rust](https://rustup.rs) stable

本專案綁定 Win32 / WinRT API,**無法在 Linux 或 macOS 上建置**。

## 本機檢查

送 PR 前請確認這三個都過(CI 跑的是同一組):

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo build --release --locked
```

## 分支模型

| 分支 | 用途 |
|---|---|
| `main` | 永遠保持可發佈狀態,只從 `develop` 以 PR 合併進來 |
| `develop` | 日常整合分支,**PR 請發到這裡** |
| `feat/*` `fix/*` `docs/*` `refactor/*` | 短生命週期的工作分支,從 `develop` 開出 |

```bash
git checkout develop && git pull
git checkout -b feat/my-change
# ... 開發 ...
git push -u origin feat/my-change   # 然後開 PR 回 develop
```

## Commit message

採 [Conventional Commits](https://www.conventionalcommits.org/),主旨用繁體中文:

```
feat: 新增 XXX 功能
fix(vision): 修正顏色容差在深色頻道下的誤判
docs: 更新 README 的設定說明
```

常用型別:`feat` / `fix` / `docs` / `refactor` / `chore` / `test` / `perf`。

## 發版

只有維護者需要做,詳見 [CLAUDE.md](CLAUDE.md#發版部署流程):
更新 `Cargo.toml` 版號 → 合併進 `main` → 打 `vX.Y.Z` tag 並推送 →
Release workflow 自動建置並發佈。
