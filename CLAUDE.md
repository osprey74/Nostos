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

## 技術スタック（未確定・ルート選択中）

- **ルートA**: Rust / embassy（no-std）。[`canardleteer/papermono-rs`](https://github.com/canardleteer/papermono-rs) を土台に拡張
- **ルートB**: 公式 C++ Meshtastic（PlatformIO / ESP-IDF）を PaperMono variant として移植
- 選択の判断材料は [docs/APPROACH.md](docs/APPROACH.md)（Phase 0〜1 の手応えで確定）

## ライセンス注意

- 公式 Meshtastic 由来コードを取り込む場合は **GPL** を継承する。cardputerzero-apps とは
  **意図的に別リポジトリ**（ライセンス・ツールチェーン分離）。混在させないこと。

## エコシステム連携

- **cardputerzero-apps**（別リポジトリ）が最終形の CardputerZero 版を担当：
  App01（gps-logger）× App02（lora-mesh-node）融合で「携行 GPS ＋ 車内 C6L 受信 ＋ 帰路ナビ」。
- 本リポジトリで確立する「受信＋復号＋ブレッドクラム＋方位算出」ロジックを CZ 版へ移植する。
- 距離・方位算出（haversine / bearing）は言語非依存で共通化。

## 現状

- 雛形段階（未着手）。技術調査を docs/ に集約済み。実装ルートは未確定。
- 次の一手候補: papermono-rs で C6L の毎分 LongFast 受信を追試（Phase 0）。

## 開発環境

- 作業パス（Windows）: `g:\dev\Nostos\`
- ESP32-S3 フラッシュ: ルートA=`cargo xtask`（papermono-rs 系）/ ルートB=PlatformIO or esptool
- 送信側 C6L の設定・技適は cardputerzero-apps の HANDOFF §3 App02 参照
