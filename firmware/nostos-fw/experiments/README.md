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

## 実機検証結果（Meshtastic phase0-1・2026-09-10・PaperMono COM8 / C6L COM7）

| portnum | 送信 | 受信・デコード結果 | 判定 |
| --- | --- | --- | --- |
| 1 (TEXT) | `--sendtext nostos-now` | `from=f115bbe0 port=1 plen=10` / payload=`6e 6f 73 74 6f 73 2d 6e 6f 77`="nostos-now" | ✅ **AES-CTR 復号一致** |
| 3 (POSITION) | `--setlat 35.0 --setlon 135.0` | `port=3` / `lat_i=349962240(≈35.0) lon_i=1349779456(≈135.0) alt=Some(0)` | ✅ **Position デコード一致**（precision=13 量子化） |

- `from=f115bbe0` = C6L ノード番号（0xF115BBE0）と一致。
- rssi -24〜-31 dBm / snr +6 dB（卓上・至近）。
- これにより **HW→RF→フレーム→復号→protobuf→lat/lon** の全段が実機実証された。

## 実機検証結果（Nostos-native Step D・2026-09-11・PaperMono COM8 / C6L COM7）

**生 LoRa の C6L→PaperMono E2E を実機で確定。** 屋内で GPS fix 不能のため、C6L を
`env:c6l-beacon-dummy`（`-DDUMMY_GPS=1`）でビルドし、送信 seq に応じて基準点 (35.0, 135.0) から
約 70m/送信・北東へ進む合成トラックを fix=1 で送信（RF は本番と完全同一・技適に無関係）。

| 観測点 | 結果 | 判定 |
| --- | --- | --- |
| C6L 送信（COM7） | `923.000MHz BW125 SF9 sync0x3A` / `TX(move) seq=1 lat_e7=350004500 lon_e7=1350005500 st=0` | ✅ 送信成功・LBT free |
| PaperMono 受信カード（e-ink 目視） | `923.000MHz / -56dBm / 11dB / 16bytes / 01 01 37 4c` | ✅ **バイト一致** |

先頭 4B `01 01 37 4c` を `version｜flags｜seq｜lat_e7…` で解読：
`version=01`・`flags=01`(FIX_VALID)・`seq=0x37=55`・`lat_e7 の LSB=0x4C=76`。
ダミー seq55 の `lat_e7 = 350000000 + 55×4500 = 350247500`、`350247500 mod 256 = 76 = 0x4C` と**完全一致**。
→ **HW→RF→16B NostosFrame→version/flags/seq/lat_e7 デコード**が実機で一致実証された。

- rssi -56 dBm / snr 11 dB（卓上・至近）。

### シリアル数値（`rxtest` で hands-off 捕捉・2026-09-11）

`rxtest` 受信FW を `espflash flash --monitor` で焼いて捕捉（タップ不要）。全フィールドがダミー生成値と一致：

```text
nostos-rx: seq=78 fix=1 lat_e7=350351000 lon_e7=1350429000 time=1789004680 rssi=-57 snr=11 len=16
nostos-rx: trail=1 home_dist_m=0   home_bearing_deg=0
nostos-rx: seq=79 fix=1 lat_e7=350355500 lon_e7=1350434500 time=1789004740 rssi=-56 snr=10 len=16
nostos-rx: trail=2 home_dist_m=70  home_bearing_deg=225
nostos-rx: seq=80 fix=1 lat_e7=350360000 lon_e7=1350440000 time=1789004800 rssi=-57 snr=11 len=16
nostos-rx: trail=3 home_dist_m=141 home_bearing_deg=225
```

- lat/lon/time が `35e7+seq×4500 / 135e7+seq×5500 / 1789000000+seq×60` と**完全一致**。
- **homing 距離** 70m→141m（1歩≈70m の等差）＝ `nostos-nav` haversine 正。
- **homing 方位 225°(SW)** ＝ 北東へ進む合成トラックに対し出発点（trail=1）が南西＝ bearing 正。
- → **RF受信→16Bデコード→Trail→haversine→bearing** の全段を実機で数値実証。

> ⚠️ シリアル観測の注意: 素の `System.IO.Ports` で COM8 を開くと ESP32-S3 の USB-Serial-JTAG が
> DTR/RTS で download モードへ落ち CDC 無音・アプリ停止になる。観測は **`espflash flash --monitor`**
> か、お手元の `pio device monitor -p COM8 -b 115200`（DTR/RTS off）で。`espflash monitor` 単体
> （既定 `--before default-reset`）も download 落ち、`--before no-reset` は稼働アプリに同期できず不可。
> 復帰は `espflash reset --port COM8`。

### タップ不要の連続受信モード（`rxtest` feature・2026-09-11 追加）

UI の LoRa カードで RX タッチする代わりに、起動直後から `listen_rx` をループして
`nostos-rx:` を吐き続ける受信専用ビルド。papermono-rs 側に追加：

- `firmware/embassy-debug/Cargo.toml`: `rxtest = ["c153", "touch"]`。
- `firmware/embassy-debug/src/main.rs`: `probe_and_park` 直後に `#[cfg(feature="rxtest")]` の
  発散ループ（`listen_rx(&mut i2c, &btn_a, &btn_b, &tp)`）。panel/ui/heartbeat は迂回される。

```powershell
# papermono-rs 上で export-esp 相当を通してから
cargo +esp build -p embassy-debug-fw --profile release-fw `
  --target xtensa-esp32s3-none-elf '-Zbuild-std=core,alloc' `
  --no-default-features --features 'c153,touch,rxtest'
espflash flash --port COM8 --monitor target\xtensa-esp32s3-none-elf\release-fw\embassy-debug-fw
```

## 実機検証結果（nostos-fw 本実装・2026-09-13・PaperMono COM8 / C6L COM7）

パッチ運用（papermono-rs 改造）を卒業し、**スタンドアロンの `firmware/nostos-fw`** で
起動 → SX1262 連続 camp → 受信 → デコード → Trail → e-ink 軌跡マップまでを実機確認。

```text
nostos-fw: bring_up pm1=1 ioe_addr=Some(79)
nostos-fw: sx get_status raw=0xa2
nostos-fw: sx1262 probe=1
nostos-fw: rx camp 923.000MHz BW125 SF9 sync 0x3A
nostos-rx: seq=61 fix=1 home=0 lat_e7=350274500 lon_e7=1350335500 ... rssi=-54 snr=10 len=16
nostos-rx: trail=2 home_set=0 home_dist_m=70  home_bearing_deg=225
nostos-rx: trail=8 home_set=0 home_dist_m=495 home_bearing_deg=225   # seq61〜68 連続・欠落ゼロ
```

開発中に踏んだ SX1262 の罠 2 件（再発防止のため記録）:

1. **コールドブート直後の GET_STATUS は raw=0xAA**（StbyRc＋command 実行失敗）を返し、
   `is_ok()` 判定だと偽陰性。→ **SetStandby(RC) 発行後に `is_standby()` で判定**（raw=0xA2）。
2. **RX continuous 中の再アーム**を SetBufferBaseAddress→SetRx だけで行うと、2 パケット目
   以降の読み出しがずれ NostosFrame CRC8 不一致（LoRa CRC は通過）。→ **SetStandby を挟む**。

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
