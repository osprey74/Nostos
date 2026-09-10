# 実装アプローチ

## 目的（PaperMono 版）

C6L が **Meshtastic で毎分ブロードキャストする Position パケット（時刻＋緯度経度）を受信**し、
過去座標をグリッドにプロット・線で結んで**移動履歴を e-ink 表示**する。将来は帰路方向も。

## 前提となる既存資産（車輪の再発明を避ける）

- **`canardleteer/papermono-rs`** — PaperMono の Rust(embassy) ハードウェア立ち上げ。
  **SX1262 で Meshtastic LongFast フレームの実機受信を確認済み**＋ピン/電源/アンテナ配線を文書化。
  → ハード層（最大の未知数）はほぼ解決済み。詳細 [HARDWARE.md](HARDWARE.md)。
- **公式 `meshtastic/firmware`** — C++ 実装。ESP32-S3＋SX1262＋e-ink をサポート（`heltec_wireless_paper` が近い雛形）。
  ただし **PaperMono variant は未整備**。
- **`bmorcelli/Launcher`** — PaperMono 対応（画面/タッチ）だが Meshtastic 機能なし。アプリ土台/フラッシュ手段として。

## 2 つのルート

### ルート A：papermono-rs を拡張（Rust / embassy）
- 既に「受信（デモジュレーション）」まで動く Rust 基盤に、**Meshtastic の復号＋解析**を足す：
  1. LongFast フレーム受信（済）
  2. Meshtastic パケットヘッダ（to/from/id/flags）を解釈
  3. **チャネル鍵で AES-CTR 復号**（既定 public チャネルは既知の default PSK）
  4. protobuf（MeshPacket→Data→Position）を解析し 時刻/緯度経度 を取り出す
  5. e-ink にプロット
- **向き**: 受信専用・目的特化に最短。Rust 志向。ハード層が済んでいる利点大。
- **難所**: AES＋protobuf＋e-ink 描画を Rust no-std で実装。

### ルート B：公式 C++ Meshtastic を移植（新 variant）
- papermono-rs のピン地図で `variants/esp32s3/m5stack_papermono/`（`variant.h`＋`platformio.ini`）を作成。
  IO エキスパンダ(M5IOE1)経由の RESET/アンテナ、PMIC(M5PM1)給電を variant で実装。
- **向き**: フル Meshtastic スタック（チャネル管理・双方向・ノードDB・UI）が欲しい場合。
- **難所**: IO エキスパンダ制御の作り込み＋**SSD1677 e-ink ドライバ**（Meshtastic の表示層対応）。

## 推奨：段階的に

| Phase | 内容 | 判断 |
| --- | --- | --- |
| 0 | ハード確認：papermono-rs で C6L の毎分 LongFast を実機受信できるか追試 | どちらのルートでも土台 |
| 1 | **表示なし**で「受信→復号→時刻/座標を取り出す」を通す（シリアル出力で検証） | ルートA が近道の可能性 |
| 2 | e-ink にグリッド＋ブレッドクラム描画 | SSD1677 描画 |
| 3 | 帰路方向・距離の算出＋表示（haversine＋方位角） | ロジックは CZ 版と共通 |

> ルートは Phase 0〜1 の手応えで確定してよい。まずは「PaperMono で C6L の位置が取れる」を最優先。

## CardputerZero 版（最終形・別リポジトリ）

`cardputerzero-apps` の **App01（gps-logger）× App02（lora-mesh-node）融合**として実装。
本リポジトリで確立する「Meshtastic 位置受信＋ブレッドクラム＋帰路方位」ロジックを移植する。
距離・方位の算出（haversine / bearing）は言語非依存で共通化できる。
