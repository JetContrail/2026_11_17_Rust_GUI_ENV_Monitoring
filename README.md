# 2026_11_17_Rust_GUI_ENV_Monitoring

Rust 製の鍵管理・環境監視プロトタイプ (Windows ネイティブ GUI)。

## 構成

| パス | 役割 |
|------|------|
| `crates/envmon-core` | GUI 非依存のドメインロジック (しきい値判定など)。どの OS でも `cargo test` 可能 |
| `crates/envmon-app` | egui / eframe 0.37 による GUI (`envmon` バイナリ)。UI 状態と描画は `egui_kittest` でヘッドレステスト |

GUI フレームワークは **egui (eframe)** を採用。即時モード GUI のため、カスタム描画 (`egui::Painter`) やマップ表示、スクリプト連携で状態を外部から操作する構成と相性がよく、`egui_kittest` によりウィンドウ / GPU なしで UI の操作テストができる。

## 設計資料

- [要求仕様 (案)](docs/requirements.md)
- [アーキテクチャ (案)](docs/architecture.md)

## 開発環境 (Linux / Ubuntu 22.04 で確認)

```sh
# Rust stable (rust-toolchain.toml により rustfmt / clippy / windows-gnu ターゲットも自動導入)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
rustup target add x86_64-pc-windows-gnu

# Windows (GNU) 向けクロスリンカ
sudo apt-get install -y gcc-mingw-w64-x86-64
```

## よく使うコマンド

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked                      # ヘッドレスで UI テストも実行
cargo build --release --locked --target x86_64-pc-windows-gnu
# => target/x86_64-pc-windows-gnu/release/envmon.exe
cargo run -p envmon-app                              # ローカル (デスクトップ環境) で起動
```

## CI

`.github/workflows/ci.yml` が PR / main への push で以下を実行する。

- `lint-test` (ubuntu): `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`
- `windows-gnu-cross` (ubuntu): `cargo build --target x86_64-pc-windows-gnu --release` → `envmon.exe` を artifact 化
- `windows-msvc` (windows-latest): Windows 実機で `cargo test` と MSVC リリースビルド → artifact 化
