# CLAUDE.md — Nostos

> M5Stack PaperMono 向けオフライン帰路ナビ（LoRa/Meshtastic 受信＋ブレッドクラム表示）

## プロジェクト概要

*Nostos*（νόστος＝帰郷・帰路）は、携帯圏外でも「たどった経路」と「出発点への帰路方向」を示す
オフラインナビ構想。本リポジトリはその **M5Stack PaperMono 版アプリ**を扱う。

C6L（Meshtastic 送信ノード）が毎分ブロードキャストする Position（時刻＋緯度経度）を PaperMono が
受信し、グリッドに移動履歴をプロット・線で結んで e-ink 表示する。将来は帰路方向・距離を指示。

詳細は [README.md](README.md)・[docs/](docs/) を参照。

## ターゲット

- **デバイス**: M5Stack PaperMono（full SKU `C153`：NFC/LoRa 搭載。`C153-Lite` は対象外）
- **SoC**: ESP32-S3R8 / 16MB Flash / 8MB PSRAM（組み込み・OS なし）
- **無線**: SX1262（Stamp LoRa-1262）868〜923MHz。日本運用は 920MHz 帯（JP）
- **画面**: 3.97" 480×800 4階調タッチ e-ink（SSD1677）＋フロントライト
- ハード詳細・SX1262 ピン地図は [docs/HARDWARE.md](docs/HARDWARE.md)

## 技術スタック（ルートA で確定）

- **PaperMono 受信側**: Rust / embassy（no-std）。`firmware/nostos-fw` がスタンドアロン FW。
  BSP は [`canardleteer/papermono-rs`](https://github.com/canardleteer/papermono-rs)（`g:\dev\papermono-rs`）を **path 依存**で参照
- **C6L 送信側**: C++ / pioarduino（`firmware/c6l-beacon`。公式 PlatformIO は C6 非対応）
- 通信は Meshtastic ではなく **Nostos-native 生 LoRa 16 バイトフレーム**（`crates/nostos-frame`）
- 経緯・判断材料は [docs/APPROACH.md](docs/APPROACH.md)

## ライセンス注意

- 公式 Meshtastic 由来コードを取り込む場合は **GPL** を継承する。cardputerzero-apps とは
  **意図的に別リポジトリ**（ライセンス・ツールチェーン分離）。混在させないこと。

## エコシステム連携

- **cardputerzero-apps**（別リポジトリ）が最終形の CardputerZero 版を担当：
  App01（gps-logger）× App02（lora-mesh-node）融合で「携行 GPS ＋ 車内 C6L 受信 ＋ 帰路ナビ」。
- 本リポジトリで確立する「受信＋復号＋ブレッドクラム＋方位算出」ロジックを CZ 版へ移植する。
- 距離・方位算出（haversine / bearing）は言語非依存で共通化。

## 現状

- **Phase 2 進行中**（2026-09-13）: `firmware/nostos-fw` を実機動作まで実装。
  受信（923.000MHz/BW125/SF9/sync 0x3A 連続 camp）→ NostosFrame デコード → HOME/Trail 管理 →
  e-ink 軌跡マップ（North-up グリッド・破線・連番・ズーム A/B）まで動作。
- HOME は `FLAG_HOME`（flags bit1）で C6L から共有する仕様に確定（[docs/UI.md](docs/UI.md)）。
- 未実装: 帰路ナビ画面（4 階調）・タッチタブ・設定タブ・RGB LED、C6L 側 OLED UI／HOME 長押し確定。

## 開発環境

- 作業パス（Windows）: `g:\dev\Nostos\`
- PaperMono フラッシュ: `firmware/nostos-fw` で `cargo +esp build --release` →
  `espflash flash --port COM8 --monitor`（xtask は Windows 不可。手順は firmware/nostos-fw/README.md）
- C6L フラッシュ: pioarduino（`firmware/c6l-beacon/README.md`）
- 送信側 C6L の設定・技適は cardputerzero-apps の HANDOFF §3 App02 参照
