# firmware/nostos-fw — PaperMono 受信ファーム（Phase 2 本実装）

PaperMono（ESP32-S3 / SX1262 / SSD1677）で動く **Nostos 受信機本体**。
C6L ビーコンの 16 バイト NostosFrame（923.000 MHz / BW125 / SF9 / sync 0x3A）を
連続受信し、North-up グリッドにブレッドクラム軌跡を e-ink 描画する。**受信専用（送信なし＝技適対象外）**。

```text
SX1262 RX 連続 camp（lora.rs）
  └─ nostos_frame::NostosFrame::decode
       ├─ FLAG_HOME → 出発点を設定/更新（座標が変われば Trail リセット）
       └─ FIX_VALID → nostos_nav::Trail::push
            └─ draw.rs（軌跡マップ）→ panel.rs（SSD1677 OTP モノクロ・部分更新バジェット）
```

## 構成

| ファイル | 役割 | 出自 |
| --- | --- | --- |
| `src/main.rs` | bring-up・受信ループ・状態管理 | 新規（embassy-debug の起動順を踏襲） |
| `src/ioe.rs` | M5IOE1・電源レール（IP2315 隔離・EPD_VDD） | papermono-rs `ioe.rs`+`touch_bus.rs` 最小移植（MIT） |
| `src/lora.rs` | SX1262 受信専用ドライバ（連続 RX・再アーム） | papermono-rs `lora.rs::listen_rx` 移植 |
| `src/panel.rs` | SSD1677 OTP モノクロ（フル/部分・DC バイアス保護） | papermono-rs `panel.rs` 移植 |
| `src/draw.rs` | 軌跡マップ描画（`docs/UI.md` 第1画面） | 新規（GrayInk パターン踏襲） |

BSP（`m5stack-papermono-lite` / `m5stack-papermono`）は `g:\dev\papermono-rs` を **path 依存**で参照。
共通ロジックは本リポジトリの `nostos-frame` / `nostos-nav`。

## 操作（現状）

- **ボタン A（上）**: ズームイン / **ボタン B（下）**: ズームアウト（1〜100 m/px の 7 段階）
- 画面更新は「フレーム受信・ズーム変更・3 分無受信」時。部分更新 18 回ごとに自動フル更新。

## ビルド / フラッシュ（Windows）

```powershell
# esp 環境（espup）を通す
. C:\Users\ospre\export-esp.ps1
cd g:\dev\Nostos\firmware\nostos-fw
cargo +esp build --release   # target/build-std/linkall は .cargo/config.toml が供給

espflash flash --port COM8 --monitor target\xtensa-esp32s3-none-elf\release\nostos-fw
```

> ⚠️ シリアル観測は `espflash flash --monitor`（または DTR/RTS off の `pio device monitor`）で。
> 素の SerialPort で COM8 を開くと USB-Serial-JTAG が download モードへ落ちる（詳細は
> [`experiments/README.md`](experiments/README.md) の注意書き）。

## 送信側（テスト）

`firmware/c6l-beacon` を COM7 へ。屋内 GPS なしなら `env:c6l-beacon-dummy`（合成トラック送信）。

## 未実装（次フェーズ）

- 帰路ナビ画面（第2画面・薄墨の来た道 → `paint_gray` 4 階調の追加が必要）
- 下部タブ（タッチ）・設定タブ（フロントライト 5 段階）・RGB LED 通知
- C6L 側 HOME 長押し確定＋`FLAG_HOME` 送出（プロトコルは実装済み・C6L UI 未着手）

## 参照

- 仕様: [`../../docs/UI.md`](../../docs/UI.md) / 配線・周波数: [`../../docs/PHASE0.md`](../../docs/PHASE0.md)
- 実機実証記録: [`experiments/README.md`](experiments/README.md)
