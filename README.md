# Nostos

> *νόστος* — 帰郷・家路への旅。オフラインの LoRa/Meshtastic を使った「出発点（車）まで戻る」簡易ナビゲーション。
>
> ⚖️ **電波法順守は絶対条件です。** 送信ファームは技適の認証範囲内に固定します。
> 厳守事項は [docs/COMPLIANCE.md](docs/COMPLIANCE.md)（922〜923.4MHz / ≤200kHz / ≤5.0mW・受信は対象外）。

## コンセプト

携帯電話の電波が届かない山中でも、**たどった経路（ブレッドクラム）と出発点への帰路方向**を示すオフラインナビ。

- **移動する人**が GPS 付き端末を携行し、自分の移動履歴を記録
- **出発点（車内など）に固定した LoRa ノード**が自身の GPS 位置を定期送信
- 携行端末が両者の位置を突き合わせ、**移動ルートの表示**と**帰路方向（方位・距離）の指示**を行う

## このリポジトリの範囲：PaperMono 版アプリ

本リポジトリは **M5Stack PaperMono（ESP32-S3 / SX1262 / 3.97" e-ink）** 上のアプリを扱う。

1. **C6L からの分ごと Meshtastic 通報を受信**（Position パケット：時刻＋緯度経度）
2. グリッドに過去の計測座標を**プロット**、**線で結んで移動履歴（経路）を e-ink 表示**
3. （発展）出発点への**帰路方向・距離**を表示

＝「Meshtastic 位置受信＋ブレッドクラム描画」ロジックの実証機。

## エコシステム連携（最終形）

本アプリで確立した受信＋描画ロジックを、**M5Stack CardputerZero** 側の融合アプリへ展開する：

- 登山・ハイキングで **CardputerZero を携行**（自機 GPS＝App01 gps-logger）
- **車内に C6L を設置**（車の位置を Meshtastic 送信）
- CZ が **App02（lora-mesh-node）の LoRa で C6L の位置を受信** → 自分の移動履歴ルート＋**車への帰路ナビ**

→ CardputerZero 側アプリは [`cardputerzero-apps`](../cardputerzero-apps/) リポジトリ（App01 × App02 融合）が担当。
Nostos（本リポジトリ）はその **PaperMono プロトタイプ兼構想の名**。

## 現状

- 未着手（雛形段階）。技術調査は [`docs/`](docs/) に集約。
- PaperMono 用 Meshtastic 実装は既製品が無く、`canardleteer/papermono-rs`（実機で Meshtastic フレーム受信を確認済み）を土台にできる。詳細は [docs/APPROACH.md](docs/APPROACH.md)。

## ターゲット / スタック

- **デバイス**: M5Stack PaperMono（full SKU `C153`：NFC/LoRa 搭載）
- **SoC**: ESP32-S3R8 / 16MB Flash / 8MB PSRAM
- **無線**: SX1262（Stamp LoRa-1262）868〜923MHz
- **画面**: 3.97" 480×800 4階調タッチ e-ink（SSD1677）＋フロントライト
- **候補ルート**: (A) Rust/embassy（papermono-rs 拡張） / (B) 公式 C++ Meshtastic 移植 — [docs/APPROACH.md](docs/APPROACH.md)
