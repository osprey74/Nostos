# firmware/c6l-beacon — Nostos C6L 生 LoRa ビーコン

C6L（ESP32-C6 ＋ SX1262 ＋ GPS）を **Meshtastic ではなく自前の生 LoRa** で動かし、
GPS の位置＋時刻を **毎分 `NostosFrame`（16B）で送信**するビーコン。受信は PaperMono（別機・受信専用）。

> ⚖️ **電波法順守は絶対条件。** 送信 RF は技適の認証枠に固定（[../../docs/COMPLIANCE.md](../../docs/COMPLIANCE.md)）。
> C6L 認証（211-250603）: F1D / 922〜923.4MHz / 200kHz / 1.5〜5.0mW。
> 既定送信: **923.000MHz / BW125 / +6dBm / SF9 / CR4-5 / sync 0x3A**（`crates/nostos-frame::radio` と一致）。

## ピンマップ（出典: meshtastic `variants/esp32c6/m5stack_unitc6l/variant.h` ＋ M5Unified）

| 機能 | ピン |
| --- | --- |
| SX1262 SPI | SCK=20 / MISO=22 / MOSI=21 / CS=23 |
| SX1262 割込み・状態 | DIO1=7 / BUSY=19 / RESET=なし(NC。実体は PI4IO P7) |
| SX1262 RF/クロック | DIO2=RFスイッチ / DIO3=TCXO 3.0V |
| OLED SSD1306 64×48 | **SPI 接続**（SX1262 とバス共有）CS=6 / DC=18 / RST=15。表示は 180°回転（U8G2_R2） |
| GPS UART | RX=4（ESP受信）/ TX=5、9600bps |
| 内部 I2C | SDA=10 / SCL=8 → **PI4IOE5V6408 エキスパンダ（0x43）** |
| 正面ボタン | **PI4IO P0（active-low）**。GPIO 直結ではない（variant.h の `BUTTON_PIN 9` はコメントアウト） |
| ブザー | GPIO11 |
| NeoPixel RGB ×1 | GPIO2 |

## 構成

- `include/nostos_frame.h` — Rust `crates/nostos-frame` と**バイト一致**の C ミラー（encode＋CRC8）。
- `src/main.cpp` — RadioLib(SX1262) ＋ TinyGPSPlus。下記の送信ロジック。
- `platformio.ini` — ESP32-C6 / Arduino / RadioLib（USB CDC ログ有効）。

## 送信ロジック（LBT＋適応間隔＋ボタン）

- **毎送信の前に RSSI キャリアセンス(LBT)**：`startReceive → getRSSI(false) → -80dBm 未満なら送信`。
  ⚠️ RadioLib は `getRSSI(true)`=last-packet（未受信で無効値 -0.5）/ `getRSSI(false)`=GetRssiInst 瞬時 RSSI。
  **必ず false**。混雑（≥-80dBm）ならバックオフ再試行（ARIB STD-T108 §3.4.2 エネルギー検出）。
  送信成功で `PIN_BUZZER` がビープ（動作確認用）。
  ※卓上は近接機器のノイズでフロアが高く「busy」になりやすい。離隔・クリーン給電で -80dBm を下回れば送信（実機検証済み）。
- **適応間隔**（固定/移動 両対応）：
  - **移動時**（前回送信位置から `MOVE_THRESHOLD_M`=30m 以上）→ 最短 `MIN_INTERVAL`=30s ごと。
  - **停止時** → `MAX_INTERVAL`=10分ごとのハートビートのみ（送信・LBT・電力を最小化）。
- **正面ボタン**（PI4IO P0・実機確認済み 2026-09-13）:
  - **短押し** → OLED ページ送り（STATUS → POSITION → HOME）
  - **ダブルクリック** → 任意発信（LBT を通す）
  - **長押し 2s** → **HOME（出発点）確定**: 現在地を保存・seq リセット・`FLAG_HOME` フレーム即時送信。
    以降は通常送信 10 回ごとに HOME を自動再放送（後起動の受信機対策）
- **ブザー**: 送信成功=ピッ / fix 獲得=ピピッ（高2）/ LBT busy=ププッ（低2）/ HOME 確定=ピーッ（長）。
  ハートビート送信は無音・無灯。
- **NeoPixel**: GPS 探索中=青遅点滅 / 送信成功=緑 300ms / LBT busy=赤 300ms / HOME 確定=白 1s。

閾値・間隔・移動しきい値は `src/main.cpp` 冒頭の定数で調整。**RF（周波数/帯域/出力）は認証枠固定・変更禁止**。

> ⚠️ LBT 閾値(`CS_THRESHOLD_DBM`)・リッスン窓は保守的な既定値。**ARIB STD-T108 と C6L 認証カテゴリ
> （キャリアセンス要否・閾値・休止時間・免除の有無）を最終確認**すること（M5Stack/総務省）。

## ⚠️ フラッシュ前の必須事項

1. **C6L はバックアップ済みであること**（`backups/c6l-meshtastic-original-*.bin` ＋ 設定 YAML）。
   Meshtastic へ戻す: `espflash write-flash 0 backups/c6l-meshtastic-original-2026-09-10.bin`。
2. **技適順守**: `src/main.cpp` の周波数・帯域・出力を認証枠外に変更しない。
   ARIB STD-T108 の LBT（キャリアセンス）は `TODO` を実装してから常用すること。

## ビルド / フラッシュ（次段）

```bash
# ビルド（初回は arduino-esp32 + RadioLib + U8g2 + NeoPixel を取得）
pio run -e c6l-beacon
# 書き込み（C6L=COM7）
pio run -e c6l-beacon -t upload --upload-port COM7
# シリアル確認（seq/fix/lat/lon/tx を出力）
pio device monitor -p COM7 -b 115200
```

> ⚠️ `pio ... -t upload` が無出力でハングすることがある（2026-09-13 遭遇）。その場合は
> pio ビルド済みの合成イメージを espflash で直接書く:
> `espflash write-bin --port COM7 0x0 .pio\build\<env>\firmware.factory.bin` → `espflash reset --port COM7`
>
> ESP32-C6 + Arduino が公式 platform で未対応の場合は `platformio.ini` の `platform` を
> pioarduino フォークに差し替える（コメント参照）。

## 受信側

PaperMono を **923.000MHz / BW125 / SF9 / CR4-5 / sync 0x3A** に設定して `NostosFrame` を受信・
`nostos-frame` でデコード → SD 記録 → e-ink 軌跡プロット（Step D）。
