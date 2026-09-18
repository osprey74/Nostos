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

- **Phase 2 実装完了・屋外フィールドテスト済み**（2026-09-18）: [`firmware/nostos-fw`](firmware/nostos-fw/)
  （Rust/embassy スタンドアロン受信 FW）で Nostos-native 生 LoRa（923.000MHz / 16 バイトフレーム）の連続受信 →
  デコード → HOME/Trail 管理 → e-ink 軌跡マップ／帰路ナビ／設定（3 タブ・日本語表示・RGB LED・microSD ログ・
  軌跡の記録 一時停止）まで実装。送信側は [`firmware/c6l-beacon`](firmware/c6l-beacon/)（LBT 準拠・OLED UI・
  HOME 長押し確定）。実 GPS で往復約 102 km・受信欠落ゼロを確認（2026-09-17）。
- 残り: CardputerZero 版への移植（cardputerzero-apps）。
- 実機実証の記録は [`firmware/nostos-fw/experiments/README.md`](firmware/nostos-fw/experiments/README.md)。

## ターゲット / スタック

- **デバイス**: M5Stack PaperMono（full SKU `C153`：NFC/LoRa 搭載）
- **SoC**: ESP32-S3R8 / 16MB Flash / 8MB PSRAM
- **無線**: SX1262（Stamp LoRa-1262）868〜923MHz
- **画面**: 3.97" 480×800 4階調タッチ e-ink（SSD1677）＋フロントライト
- **確定ルート**: (A) Rust/embassy。BSP は papermono-rs を path 依存で利用 — [docs/APPROACH.md](docs/APPROACH.md)
