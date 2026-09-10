# Phase 0 / 1 — 受信ロジックの差分分析

> 土台: [`canardleteer/papermono-rs`](https://github.com/canardleteer/papermono-rs)（MIT / `g:\dev\papermono-rs` に取得済み）
> 精読日: 2026-09-10（commit `67ee512`）

## 結論

papermono-rs の `firmware/embassy-debug/src/lora.rs::listen_rx()` が **Phase 0 の大部分を既に達成**している。
SF11 / BW250 / CR4-5・Meshtastic sync word (`0x24B4`) で LongFast フレームを実受信し、
RSSI / SNR / 長さ / 先頭バイトまで取得できる。**ハード層（最大の未知数）は解決済み**。

Nostos が足すのは「**フルペイロード読み出し → 復号 → protobuf 解析 → 時刻/緯度経度 抽出**」の 4 段。
このうち復号・protobuf は**実機なしでホスト単体テスト可能**（本リポジトリの `nostos-meshtastic` crate）。

## papermono-rs から流用できる資産（MIT）

| 資産 | 場所 | Nostos での扱い |
| --- | --- | --- |
| SX1262 ドライバ（`Sx1262`・全コマンド） | `crates/m5stack-papermono/src/lora.rs` | git 依存でそのまま利用 |
| LoRa 定数（SF/BW/CR/sync/IRQ） | 同上 | そのまま利用 |
| M5PM1 / M5IOE1 ドライバ・I2C アドレス | `crates/m5stack-papermono-lite/` | git 依存で利用 |
| 電源シーケンス（`power_up`/`power_down`） | `firmware/embassy-debug/src/lora.rs` | bin ローカルなので**移植**（Phase 2） |
| I2C 立ち上げ・IOE 制御 | `firmware/embassy-debug/src/ioe.rs`・`board.rs` | 同上・移植 |
| 受信ループ（`listen_rx`） | 同上 | **拡張の起点**（下記） |

## ハード配線（listen_rx が使う実配線・確定値）

| 信号 | 接続 | 定数 |
| --- | --- | --- |
| SPI3 | MOSI=GPIO38 / SCK=GPIO39 / MISO=GPIO40 / NSS=GPIO41 @ 8MHz | `SPI_USERDEMO_HZ` |
| BUSY | GPIO21 | — |
| IRQ / DIO1 | GPIO5 | — |
| LoRa 電源 | M5PM1 `G2`（`3V3_L2_LoRa` レール） | `PMIC_ENABLE` |
| RESET | M5IOE1 `PYG10`（active low） | `IOE1_RESET` |
| アンテナ切替 | M5IOE1 `PYG2`（HIGH=内蔵FPC接続） | `IOE1_ANTENNA_SWITCH` |

起動順（`power_up`）: ①RESET=LOW → ②アンテナ=HIGH → ③PMIC `G2`=HIGH → ④15ms → ⑤RESET=HIGH → ⑥20ms。

## RX モデム設定（listen_rx 実測・LongFast）

```text
set_packet_type(LORA)
set_rf_frequency(target)                 // ← Nostos で JP 920MHz に変更（下記）
set_lora_modulation_params(SF11, BW250, CR4-5, LDRO_OFF)
set_lora_packet_params(preamble=16, HEADER_VARIABLE, 255, CRC_ON, IQ_STANDARD)
set_lora_sync_word(0x24B4)               // SYNC_WORD_MESHTASTIC
set_rx(0xFFFFFF)                         // continuous
```

## ⚠️ 差分 1：周波数を US915 → JP へ（実測確定済み）

papermono-rs の検証は **US915**（`FREQ_RX_SNIFFER_PRI_HZ = 917.625MHz` 等）。
**日本運用は JP** に変更が必須。C6L（Region=JP / LONG_FAST / 既定チャネル）の中心周波数：

- **JP LongFast 中心周波数 = `923.375 MHz`（`923_375_000` Hz）** ← 2026-09-10 実受信で確定。
- 導出（Meshtastic firmware 一次情報）: JP region `freqStart=920.5 / freqEnd=923.5 / spacing=0` →
  `numChannels = floor((923.5-920.5)/0.25) = 12`。既定 primary は空名→preset 名 `"LongFast"`、
  `hash("LongFast") % 12 = ch 11`（djb2）→ `freq = 920.5 + 0.125 + 11*0.25 = 923.375`。
  （`"LongFast"`/`"Long Fast"` どちらでも ch 11 に収束し同値。）
- `set_rf_frequency(923_375_000)`。sync word `0x24B4`・SF11/BW250/CR5 は LongFast 共通（C6L 設定と一致）。

### 実受信ログ（Phase 0 ② 実機確認・2026-09-10）

PaperMono(COM8) を 923.375MHz に camp、C6L(COM7) から `--sendtext` した LongFast を受信：
**len=32B / RSSI=-29dBm / SNR=+6dB / preview=`ff ff ff ff`**。
preview 先頭4B = dest `0xFFFFFFFF`（ブロードキャスト）＝ `nostos-meshtastic::MeshHeader` の想定と一致。
→ 受信チェーン（HW→RF→フレーム構造）が実機で実証された。

## ⚠️ 差分 2：フルペイロード読み出し

現状 `listen_rx` は **先頭 4 バイトのプレビューのみ** (`read_buffer(start_ptr, &mut [0u8;4])`)。
Nostos は `get_rx_buffer_status()` が返す `len` 全体を読む：

```rust
let (len, start_ptr) = sx.get_rx_buffer_status()?;   // len = 実バイト数
let mut payload = [0u8; 256];
sx.read_buffer(start_ptr, &mut payload[..len as usize])?;
```

以降は `nostos-meshtastic` に渡す（実機不要でここから先はホスト検証済みロジック）。

## 差分 3：Meshtastic フレーム解析（→ `nostos-meshtastic` crate）

オンエア形式（protobuf ではなく生ヘッダ + 暗号化ペイロード）:

```text
[ dest u32 LE ][ from u32 LE ][ packet_id u32 LE ][ flags u8 ][ ch_hash u8 ][ next_hop u8 ][ relay u8 ]  = 16B ヘッダ
[ AES-CTR 暗号化ペイロード ... ]
```

復号後ペイロードは `Data` protobuf（`portnum`, `payload`）。`portnum == POSITION_APP(3)` のとき
`payload` は `Position` protobuf（`latitude_i` / `longitude_i` = 度 ×1e7、`time` = unix秒 ほか）。

### 既定チャネル鍵（public/LongFast）

- チャネル PSK が 1 バイト `0x01`（base64 `AQ==`）のとき = **既定鍵を使う**の意。
- 既定鍵（AES-128、16B）: `d4 f1 bb 3a 20 29 07 59 f0 bc ff ab cf 4e 69 01`
  （base64 `1PG7OiApB1nwvP+rz05pAQ==` をデコード。2026-09-10 検証）
- 確実性: 中。実配信フレームで最終確認すること（meshtastic/firmware `CryptoEngine`）。

### AES-CTR nonce（初期カウンタブロック 16B）

```text
nonce[0..8]   = packet_id を u64 little-endian
nonce[8..12]  = from(送信ノード) を u32 little-endian
nonce[12..16] = 0（ブロックカウンタ、BE インクリメント）
```

RustCrypto の `Ctr128BE<Aes128>` に IV=nonce で一致（ペイロードは短くカウンタは下位 4B 内で完結）。
確実性: 中。`CryptoEngine::initNonce` と突合すること。

## Phase 段階と検証手段

| Phase | 内容 | 検証 |
| --- | --- | --- |
| 0 | JP 周波数で C6L の毎分 LongFast を実受信（RSSI/長さ確認） | 実機 + `cargo xtask monitor`（CDC ログ） |
| 1 | フルペイロード → 復号 → Position 抽出（時刻/緯度経度をシリアル出力） | **`nostos-meshtastic` はホスト `cargo test` で先行検証** → 実機で結線確認 |
| 2 | e-ink にグリッド＋ブレッドクラム描画（SSD1677） | 実機目視 |
| 3 | 帰路方向・距離（haversine/bearing） | **`nostos-nav` はホスト `cargo test` で検証済み** → 実機表示 |

> Phase 1 の「難所（AES＋protobuf）」と Phase 3 の算出ロジックは**実機を待たずホストで確定**できる。
> 実機依存（電源シーケンス・RX 結線・e-ink）だけを後段に残す設計。
