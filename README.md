# 2026_11_17_Rust_GUI_ENV_Monitoring

Rust 製の入退室管理・環境監視・鍵管理・電源監視の統合ソフトウェア (Windows ネイティブ GUI)。

## 構成

| パス | 役割 |
|------|------|
| `crates/core` (`envmon-core`) | ドメインモデル、ユーザ管理 (SQLite + メモリ索引、20 万人)、オブジェクト状態 (5,000 個)、連動ルール、ドア監視 (こじ開け / 開放放置警報) |
| `crates/protocol` (`envmon-protocol`) | Ethernet/IP 通信層: TLS 1.3 相互認証、AES-256-GCM 電文暗号化、カードリーダ電文パーサ、機器トレイト (リーダ / 電気錠 / 環境監視 / 鍵保管器 / PDU)、ホスト側処理 |
| `crates/simulator` (`envmon-sim`) | 仮想機器シミュレータ (ライブラリ + CLI)。カードタッチ、ドア開閉・放置、温湿度変化を暗号化リンク経由で送信 |
| `crates/api` (`envmon-api`) | HTTPS (TLS 1.3) REST WebAPI。Bearer トークン + RBAC (viewer / operator / admin) |
| `crates/gui` (`envmon`) | egui / eframe 0.37 による GUI (このフェーズは起動確認用の骨格)。`egui_kittest` でヘッドレステスト |

機器との電文形式は実機仕様が未入手のため、自前のアダプタ形式 (`crates/protocol/src/frame.rs`, `message.rs`) で定義している。

GUI フレームワークは **egui (eframe)** を採用。即時モード GUI のため、カスタム描画 (`egui::Painter`) やマップ表示、スクリプト連携で状態を外部から操作する構成と相性がよく、`egui_kittest` によりウィンドウ / GPU なしで UI の操作テストができる。

## 設計資料

- [仕様書 (PO 提示)](docs/spec.md)
- [要求仕様 (実装上の解釈・前提・フェーズ)](docs/requirements.md)
- [アーキテクチャ](docs/architecture.md)

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
# => target/x86_64-pc-windows-gnu/release/{envmon,envmon-sim,envmon-api}.exe
cargo run -p envmon-gui                              # ローカル (デスクトップ環境) で GUI 起動
cargo test -p envmon-core --release --test performance -- --nocapture   # 20 万ユーザ照合 / 5,000 オブジェクト更新の性能
cargo bench -p envmon-core                           # Criterion ベンチマーク
cargo run -p envmon-simulator -- run --transport tls # デモシナリオを TLS 1.3 で実行 (--transport aes で AES-256-GCM)
cargo run -p envmon-simulator -- scenario            # シナリオ JSON の雛形を出力 (--scenario で読み込み)
cargo run -p envmon-api                              # https://127.0.0.1:8443 で WebAPI 起動 (開発用 CA と一時トークンを生成)
```

## CI

`.github/workflows/ci.yml` が PR / main への push で以下を実行する。

- `lint-test` (ubuntu): `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`
- `windows-gnu-cross` (ubuntu): `cargo build --target x86_64-pc-windows-gnu --release` → exe を artifact 化
- `windows-msvc` (windows-latest): Windows 実機で `cargo test` と MSVC リリースビルド → artifact 化
