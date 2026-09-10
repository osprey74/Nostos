# PaperMono ハードウェア（SX1262 / LoRa 中心）

> 出典: [`canardleteer/papermono-rs`](https://github.com/canardleteer/papermono-rs)（2026-09-05 実機検証）ほか M5 公式 docs。
> ⚠️ ピン割当は実装前に papermono-rs の `crates/m5stack-papermono/src/lora.rs` および
> `.agents/skills/m5stack-papermono-hardware/resources/stamp-lora-1262.md` で**再確認**すること。

## SKU

- **full PaperMono = `C153`**：NFC（ST25R3916）＋ LoRa（SX1262）搭載
- **`C153-Lite`**：NFC/LoRa **非搭載**（本プロジェクト対象外）

## SoC / 主要デバイス

| 分類 | 内容 |
| --- | --- |
| SoC | ESP32-S3R8 / 16MB Flash / 8MB PSRAM |
| 画面 | 3.97" 480×800 4階調タッチ e-ink（SSD1677）＋フロントライト |
| 無線 | SX1262（Stamp LoRa-1262）868〜923MHz / 2.4GHz WiFi / NFC ST25R3916 |
| センサ他 | BMI270 IMU / RX8130CE RTC / M5IOE1 IO エキスパンダ / M5PM1 PMIC / microSD / PDM マイク / ブザー / RGB LED / バッテリ |

## SX1262（Stamp LoRa-1262）配線 ★移植の要

papermono-rs の実機検証記録より：

| 信号 | 接続 | 備考 |
| --- | --- | --- |
| SPI | **GPIO38 / 39 / 40 / 41**（JTAG から mux） | SCK/MISO/MOSI/NSS の個別割当は lora.rs で要確認 |
| BUSY | **GPIO21** | 直結 GPIO |
| RESET | **M5IOE1（I2C IO エキスパンダ）の PYG10** | ⚠️ 直結 GPIO ではない |
| アンテナ切替 | **M5IOE1 PYG2** | HIGH=内蔵 FPC アンテナ接続 / LOW=切断 |
| 電源 | **M5PM1（PMIC）G2 レール = `3V3_L2_LoRa`** | ⚠️ ソフトで給電 ON が必要 |
| DIO1 | （要確認） | 割込み用 |

### ⚠️ 移植上の最重要注意
**RESET・アンテナ切替・LoRa 電源が「直結 GPIO ではなく I2C IO エキスパンダ(M5IOE1)／PMIC(M5PM1) 経由」**。
公式 Meshtastic（C++）の SX1262 ドライバは RESET/BUSY を直結 GPIO 前提のため、
**IO エキスパンダ制御を variant 側で作り込む必要**がある（papermono-rs は Rust で解決済み・参照可）。
起動シーケンス：①M5PM1 で `3V3_L2_LoRa` を ON → ②M5IOE1 PYG2 を HIGH（アンテナ接続）→
③M5IOE1 PYG10 で SX1262 RESET → ④SPI 初期化。

## LoRa PHY（Meshtastic 互換の実測パラメータ）

papermono-rs が **実機で Meshtastic LongFast フレーム受信を確認**した設定：

| 項目 | 値 |
| --- | --- |
| Sync Word | **論理 `0x2B`（SX1262 エンコード `0x24B4`）** ＝ Meshtastic 既定 |
| 変調 | SF11 / BW 250kHz / CR 4/5（LongFast） |
| 周波数（検証時） | 906.875 MHz（**US915**） |
| 受信実績 | 50-byte LongFast broadcast を -107dBm RSSI / -16dB SNR で受信 |

> ⚠️ **日本運用は JP 周波数**（920MHz 帯）に変更すること。検証は US915 で行われている。
> C6L 側は Region=JP / LongFast / 既定チャネル（[`cardputerzero-apps` の C6L 設定参照]）で送信。

## 依存（papermono-rs が挙げる主要ライブラリ）

M5Unified / M5PM1 / M5IOE1 / M5Unit-NFC / RadioLib（C++ 系の場合）。
