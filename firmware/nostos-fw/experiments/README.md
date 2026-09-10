# experiments/ — 実機検証パッチ（papermono-rs ベース）

`firmware/nostos-fw` を本実装する前に、**実機で受信→復号→座標抽出を実証**するために
papermono-rs の `embassy-debug` に当てた実験パッチを保存する。nostos-fw への移植時の一次参照。

## papermono-phase0-1-decode.patch

`g:\dev\papermono-rs` の `firmware/embassy-debug` に当てるパッチ（`git apply --ignore-whitespace`）。
中身：

1. **周波数を JP LongFast に固定** — `listen_rx` / `current_sniffer_freq_hz` を
   US915 から **923.375 MHz**（JP region 920.5-923.5, ch11）に camp。受信窓を 60→90 秒に拡張。
2. **フルペイロード読み出し** — 先頭4Bプレビュー → `len` 全体を 256B バッファへ。
3. **オンデバイス復号** — 本リポジトリの `nostos-meshtastic` クレート（path 依存）で
   `MeshHeader::parse → decrypt_in_place(既定鍵) → Data::parse → Position::parse`、結果をシリアル出力。

> ⚠️ `nostos-meshtastic` を papermono-rs から参照するクロスリポジトリ path 依存を含む（実験用）。
> nostos-fw 本実装では papermono-rs 側 bring-up を移植し、この path 依存は不要になる。

## papermono-nostos-receiver.patch（Step D・受信側）

stock `embassy-debug` を **Nostos-native 受信機**にするパッチ（`phase0-1` とは排他＝どちらか一方を当てる）。

1. **RX を Nostos チャネルに固定** — `listen_rx` を **923.000 MHz / BW125 / SF9 / CR4-5 / sync word 0x3A(=reg 0x34A4)**
   に設定（`firmware/c6l-beacon` / `crates/nostos-frame::radio` と一致）。フルペイロード読み出し。
2. **`nostos-frame` でデコード** — `NostosFrame::decode` → `GeoPoint` → `nostos-nav::Trail` に push →
   帰路（最古点への距離・方位）をシリアル出力（`nostos-rx: seq=… lat_e7=… rssi=… / trail=… home_dist_m=… home_bearing_deg=…`）。
3. path 依存に `nostos-frame` / `nostos-nav` を追加。**受信のみ（送信なし＝技適対象外）**。

> エンドツーエンド確認には送信側（`c6l-beacon`）が必要。ビーコンのボタン任意発信でテスト可。

## 実機検証結果（2026-09-10・PaperMono COM8 / C6L COM7）

| portnum | 送信 | 受信・デコード結果 | 判定 |
| --- | --- | --- | --- |
| 1 (TEXT) | `--sendtext nostos-now` | `from=f115bbe0 port=1 plen=10` / payload=`6e 6f 73 74 6f 73 2d 6e 6f 77`="nostos-now" | ✅ **AES-CTR 復号一致** |
| 3 (POSITION) | `--setlat 35.0 --setlon 135.0` | `port=3` / `lat_i=349962240(≈35.0) lon_i=1349779456(≈135.0) alt=Some(0)` | ✅ **Position デコード一致**（precision=13 量子化） |

- `from=f115bbe0` = C6L ノード番号（0xF115BBE0）と一致。
- rssi -24〜-31 dBm / snr +6 dB（卓上・至近）。
- これにより **HW→RF→フレーム→復号→protobuf→lat/lon** の全段が実機実証された。

## ビルド/フラッシュ（Windows・xtask 非対応のため直接）

```bash
# papermono-rs 上で、export-esp 相当の env を通してから：
cargo +esp build -p embassy-debug-fw --profile release-fw \
  --target xtensa-esp32s3-none-elf -Zbuild-std=core,alloc \
  --no-default-features --features c153,touch,panel,sleep,orient
espflash flash --port COM8 --monitor target/xtensa-esp32s3-none-elf/release-fw/embassy-debug-fw
```

詳細・周波数導出は [`../../../docs/PHASE0.md`](../../../docs/PHASE0.md)、環境の注意は
[`../../../docs/TOOLCHAIN.md`](../../../docs/TOOLCHAIN.md) を参照。
