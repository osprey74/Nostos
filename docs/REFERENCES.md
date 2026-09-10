# 参照リンク

## PaperMono / ハードウェア
- [M5Stack PaperMono 公式 docs](https://docs.m5stack.com/en/core/PaperMono)
- [canardleteer/papermono-rs](https://github.com/canardleteer/papermono-rs) — ★Rust ハード立ち上げ。Meshtastic フレーム受信実証＋SX1262 ピン地図。`crates/m5stack-papermono/src/lora.rs`・`.agents/skills/m5stack-papermono-hardware/resources/stamp-lora-1262.md`
- [m5stack/M5PaperMono-UserDemo](https://github.com/m5stack/M5PaperMono-UserDemo) — 公式ファクトリデモ（画面の LoRa Rx/Tx デモ）
- [bmorcelli/Launcher](https://github.com/bmorcelli/Launcher) — PaperMono 対応 Launcher（Meshtastic 機能なし）

## Meshtastic
- [meshtastic/firmware](https://github.com/meshtastic/firmware) — 公式 C++ ファーム。`variants/esp32s3/heltec_wireless_paper`（ESP32-S3＋SX1262＋e-ink の近い雛形）
- [m5stack/meshtastic-firmware](https://github.com/m5stack/meshtastic-firmware) — M5 fork（`m5stack_cores3` 等はあるが PaperMono variant は無し）
- [Meshtastic ビルド手順](https://meshtastic.org/docs/development/firmware/build/)
- [Heltec Wireless Paper（Meshtastic 機・雛形参照）](https://meshtastic.org/docs/hardware/devices/heltec-automation/)

## エコシステム（帰路ナビ最終形）
- `../cardputerzero-apps/` — CardputerZero 版（App01 gps-logger × App02 lora-mesh-node 融合）
- `../cardputerzero-apps/apps/lora-mesh-node/tools/rangetest_analyze.py` — 位置×受信の突合・可視化の先行例
- C6L（送信側 Meshtastic ノード）の設定・技適等は cardputerzero-apps の HANDOFF §3 App02 参照

## LoRa PHY 実測（papermono-rs 由来）
- Sync Word 論理 `0x2B` / エンコード `0x24B4`、LongFast=SF11/BW250/CR4-5、日本運用は 920MHz 帯（JP）
