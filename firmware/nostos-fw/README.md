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

- **下部 3 タブ（軌跡 / 帰路 / 設定・日本語表示）のタップ**: 画面切替
- **ボタン A / B**: 地図画面＝ズームイン/アウト（1〜100 m/px の 7 段階）、
  設定画面＝フロントライト明るさ ▲/▼（5 段階）
- **スワイプ**: 軌跡マップのパン（1 スワイプ＝1 回再描画・自動追従停止＋「PAN」表示）
- **長押し（0.8s）**: 再センタリング（最新受信点への自動追従に復帰）
- 画面更新は「フレーム受信・操作・3 分無受信」時。軌跡/設定＝モノクロ（部分更新 18 回ごと
  に自動フル）、帰路＝4 階調 GrayFull（常にフル更新）。

## 画面

1. **軌跡マップ**（モノクロ）: North-up グリッド・破線コネクタ・白抜き経由点＋連番・
   家アイコン（出発点）・現在地 ◉・スケールバー・時刻/電池/受信経過ヘッダ
2. **帰路ナビ**（4 階調）: 現在地中心固定・薄墨（LIGHT）の来た道＋レンジリング・
   **濃破線の帰路方位**・家マーカー直下に距離と方位（BRG）。HOME フレーム未受信の間は
   最初の受信点を暫定出発点とし「**HOME\***」表記（受信で正式値に置換）
3. **設定**（モノクロ）: 明るさ 5 段階（A/B）・自動消灯 30s（行タップでトグル）・
   通知 LED 凡例・BAT/VIN 電圧・FW バージョン

## 通知 LED（左側面 RGB・緑=IOE PYG8 / 青=PYG9 / 赤=PM1 LED_EN）

優先度順: **赤点滅**=低電池(<3.5V) ＞ **橙ゆっくり点滅**=受信途絶(3 分) ＞
**緑 1 回点滅**=新規フレーム受信 ＞ **青点灯**=USB 給電/充電 ＞ 消灯=待機。

## 電源（現状の制約と運用・2026-09-13）

- **起動**: USB（VIN）給電で自動起動（モバイルバッテリ可）。起動後に USB を抜いても動作は継続する
  （PM1 に LDO/DCDC 有効＋電源ホールド＋ウォッチドッグ無効を書き込むため）
- **電源オフ**: 設定タブで画面 1 秒長押し（PM1 シャットダウン）
- **バッテリ単体でのコールドブートは未解決**: CPU 起動と電源維持は成功する
  （起動ビープ「ピッ→ピピッ」で確認可）が、表示系 5V の安全な有効化方法が未確立。
  ⚠️ `PWR_CFG` の BOOST(bit3) を VIN あり状態で ON にすると 5V レール競合で表示系が
  死ぬ（実測）。安易に触らないこと
- ⚠️ **PM1 への I2C 書き込みはバス整定（500ms）後に行う**。整定前の書き込みは化けて
  別レジスタを破壊し得る（PM1 はバッテリで常時生存＝壊れた設定はリセットでも残留）
- ⚠️ **【未解決】電源断→コールドブートで SSD1677 が固着する**: 電源を切って入れ直すと
  描画がガラスに反映されなくなる（FW は正常動作・ビープ/LED は生きる）。EPD_RST／
  SW_RESET／init_mono＋RAM 自動クリア／PM1 リセット（SYS_CMD=0xA2）／レール断いずれでも
  回復せず、**工場ファームの起動だけが回復させる**。当面の運用は「**電源を切らない**」
  （USB／モバイルバッテリで連続給電。e-ink なので消費は小さい）。
- **固着時の復旧手順**（唯一の実績ある方法）:
  1. 工場イメージ書き込み: `espflash write-bin --port COM8 0x0 backups\papermono-factory-original-*.bin`
  2. 電源ボタン 2 秒長押し（赤点滅＝ダウンロードモード）→ 短押し → 工場デモ表示を確認
  3. nostos-fw を書き戻す（espflash flash）→ ウォーム状態では正常に表示される
- 調査済みの手がかり（次回継続）: M5GFX 工場ドライバとの差分は
  ①ブースタソフトスタート最終バイト（工場 `0x40`／OTP デモ由来の当方 `0x80`）
  ②`_after_wake` の RAM 自動クリア 0x46/0x47（移植済み・単独では回復せず）
  ③固着の引き金は「パネルが Deep Sleep Mode 1 のまま電源断」の疑い
- USB の抜き挿しは**画面静止時**に行うこと（更新中の電源変動が固着の引き金）

## 日本語表示

タブ・設定画面のラベルは 16×16 の 1bpp グリフ（`src/jpfont.rs`）。
文字の追加は `tools/gen_jpfont.py` の `CHARS` に追記して再生成（BIZ UDゴシックから変換）。

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

- 下部タブ（タッチ座標デコード）・設定タブ（フロントライト 5 段階）・RGB LED 通知
- 日本語ラベル（ビットマップフォント埋め込み）

※ C6L 側 UI（OLED 3 ページ・長押し HOME 確定＋`FLAG_HOME` 送出）は 2026-09-13 実機確認済み
（`firmware/c6l-beacon`）。HOME フレーム受信で HOME\* → 正式 HOME への置換も E2E 確認済み。

## 参照

- 仕様: [`../../docs/UI.md`](../../docs/UI.md) / 配線・周波数: [`../../docs/PHASE0.md`](../../docs/PHASE0.md)
- 実機実証記録: [`experiments/README.md`](experiments/README.md)
