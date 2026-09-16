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
3. **設定**（モノクロ）: 明るさ 5 段階 OFF/1/2/3/MAX（A/B・4 ボックスのバー）・自動消灯 30s（行タップでトグル）・
   通知 LED 凡例・REBOOT / SD CARD 行・BAT/VIN 電圧・FW バージョン

### パネル駆動方式（`src/panel.rs`・2026-09-16）

工場 OTP 波形のみ使用（安全契約）。モノクロ全面＝`0xF8`（反転同期）→`0x14`、部分更新＝`0xFF`（軌跡の
毎分更新・タブ切替。18 回ごとに全面で残像消去）、帰路 4 階調＝`0xD7`。黒が濃く、部分更新の残像も少ない。
タブタップ時に短いクリック音で受け付けを通知（`main.rs` の `beep_blocking(…, 15)`）。

経緯: コールドブート固着の仮説「OTP 内蔵電圧では冷えたパネルが駆動できない」を検証するため、工場 M5GFX
`Panel_SSD1677_4Gray` と同じカスタム LUT 駆動（`lut_quality`/`lut_fast`/`lut_fastest`・LUT 0x32＋駆動電圧
0x03/0x04/0x2C 明示・Mode 1 `0xC7`／Mode 2 `0xCC`）へ一度移植したが、**真因は IOE1 io3 の駒動不良で波形は
無関係**だった。実機評価も fast（0.33 秒）は黒が薄い／quality（3.4 秒）はタップを取りこぼす／fastest 差分
（0.13 秒）は残像が残る、と悪く、タブごとの使い分けやセトル再描画で補う案は複雑さに見合わないため撤回。
コードも削除した（履歴: git log 2026-09-16 `490ffcd`〜`6f6cd4d`）。文字の濃さはフォント太字化で対応。

## 通知 LED（左側面 RGB・緑=IOE PYG8 / 青=PYG9 / 赤=PM1 LED_EN）

優先度順: **赤点滅**=低電池(<3.5V) ＞ **橙ゆっくり点滅**=受信途絶(3 分) ＞
**緑 1 回点滅**=新規フレーム受信 ＞ **青点灯**=USB 給電/充電 ＞ 消灯=待機。

## 電源（運用・2026-09-16 更新）

- **起動**: USB（VIN）給電で自動起動（モバイルバッテリ可）。起動後に USB を抜いても動作は継続する
  （PM1 に LDO/DCDC 有効＋電源ホールド＋ウォッチドッグ無効を書き込むため）
- **電源オフ**: 設定タブで画面 1 秒長押し（PM1 シャットダウン）。**電源ボタン短押しで再起動でき、
  コールドブートでも正常に描画する**（2026-09-17 実機確認: 自動消灯中に電源オフ→起動でライト再点灯・
  軌跡タブ表示）。⚠️ USB を挿したままだと VIN で即再起動するため電源オフにならない
- **サイド電源ボタンの誤操作対策（2026-09-14 実機確認）**: PM1 のボタン設定で **単クリック・リセット
  （SINGLE_RST_DIS=1）とダブルクリック電源オフ（DOUBLE_OFF_DIS=1）を無効化**、長押しは 4 秒に延長
  （LONG_DLY=11）。ボタン割り込みはマスク（IRQ_MASK3）。実装は [`src/ioe.rs`] `configure_power_button()`。
  4 秒長押し→download mode は温存（DL_LOCK=0）。復旧はボタン非依存の espflash（USB）で常に可能。
- ⚠️ `PWR_CFG` の BOOST(bit3) を VIN あり状態で ON にすると 5V レール競合で表示系が死ぬ（実測）。
  安易に触らないこと
- ⚠️ **PM1 への I2C 書き込みはバス整定（500ms）後に行う**。整定前の書き込みは化けて別レジスタを
  破壊し得る（PM1 はバッテリで常時生存＝壊れた設定はリセットでも残留）

### コールドブート固着 — 真因と修正（2026-09-16 解決）

**症状**: 完全電源断（電源ボタン長押し／電池枯渇）からの起動で、CPU・PM1・LoRa・SD・タッチは動くのに
e-ink だけが前の画像のまま固まる。ウォームリセットでは再現せず、工場ファームを一度起動すると直る。

**真因**: **M5IOE1 の io3（EPD_VDD_ENABLE）がコールド起動直後、MODE=出力／OUT=1／DRV=push-pull と
登録されているのに実ピンが LOW のまま**（出力ドライバが有効化されない）。パネル（SSD1677）が無電源
なので、HW/SW リセットも RAM 書き込みも Master Activation も空振りし、BUSY が一度も上がらない
（`busy_rose=0 took=102ms`）。io5（EPD_RST）が LOW に見えたのは無電源の SSD1677 の ESD ダイオードに
クランプされていたため。**io3 を入力＋プルアップへ一度切り替えて出力に戻すと駒動が始まり**、その場で
パネルが復帰した（電源サイクル不要）。工場ファームで直るのは M5GFX/M5Unified の初期化順序が
偶然 MODE 遷移を起こすためと推測。波形（OTP か M5GFX か）は無関係だった。

**修正**: [`src/ioe.rs`] `set_output_verified()` — push-pull 出力を書いた後に **IN レジスタ（実ピン
レベル）を読み戻し**、追従しなければ `kick_pin()`（MODE→入力＋PU→出力）で振り直す（最大 3 回）。
`bring_up()` の電源イネーブル／リセット系全ピンと `panel::begin()` の EPD_VDD/EPD_RST に適用。
キックが入ると `nostos-fw: ioe1 pinN recovered by kick xK` を出す。実機: 電源ボタン全レール断→起動で
正常描画を確認（試験 #2）。

**診断出力（起動時）**: `ioe1[pre-rst]/[post-rst] mode/out/in/pu/pd/drv` ダンプと
`panel diag hw_rst / sw_rst / auto_clear46` の BUSY 挙動。`in` が `out` と一致しない電源系ピンが
あれば駒動不良。ステータスログの起動事象は `boot` / `boot_panel_fail`（初期化失敗）/
`boot_panel_nobusy`（初回描画で BUSY が上がらず）で SD に残る。

**経緯（否定された仮説・2026-09-13〜16）**: booster soft-start 0x40（工場値是正としてはコミット済み）／
EPD_VDD(io3) の LOW→HIGH サイクル（ピンが駒動されていないので効果なし＝真因の裏返し）／
明示 `_power_on`（0x22=0xC0→0x20）／5V・PM1 レール／OTP 内蔵電圧（M5GFX 駆動へ移植しても固着＝波形
無関係）。詳細は [`../../CLAUDE.md`] と git log。

- **固着時の復旧**（旧手順・原則不要になった）: 工場イメージ書き込み → 電源ボタン 4 秒→短押し →
  nostos-fw 書き戻し。修正版ファームでは起動時に自動復旧する。
- USB の抜き挿しは**画面静止時**に行うこと（更新中の電源変動を避ける）

## 日本語表示

タブ・設定画面のラベルは 16×16 の 1bpp グリフ（`src/jpfont.rs`）。
文字の追加は `tools/gen_jpfont.py` の `CHARS` に追記して再生成（**BIZ UDゴシック Bold** から変換。
e-ink では 1px の線が灰色に見えるため 2026-09-16 に太字へ変更。英数字の中サイズも同寸の
`FONT_9X15_BOLD`）。

## UI 設定の永続化（明るさ・自動消灯）

明るさ段階と自動消灯 ON/OFF は **PM1 RTC RAM（0xA0〜・電池で保持・32 バイト）**に保存する
（`ioe::save_ui_settings` / `load_ui_settings`。[0xA0]=マジック 0x5A / [0xA1]=段階 / [0xA2]=自動消灯）。
起動時に復元し、段階 >0 ならフロントライトを再点灯する（PM1 シャットダウン後は PWM 出力が止まって
いるため）。PWM デューティの読み戻し（`read_frontlight_duty`）は自動消灯中に 0 になるので、未保存時の
代用にのみ使う。起動ログ `ui settings brightness=N auto_off=B (pm1 rtc ram)`。
M5Unified／工場ファームはこの領域を使っていない（2026-09-16 確認）。

明るさの A/B ボタンは連打できる: ライトは即時に変わり、設定画面の描き直しは操作が
`BRIGHTNESS_REDRAW_DELAY_MS`=600ms 止まってから 1 回だけ行う（描画中のボタン取りこぼし防止）。

## microSD CSV ロガー（2026-09-14 実機確認）

受信した NostosFrame を **microSD に CSV 追記**するオフラインロガー。実験後に SD を抜いて
PC で回収できる（実 GPS フィールドテスト用）。

- **ハード**: SDHOST ペリフェラル（SPI2 パネル / SPI3 LoRa とは別）を GPIO マトリクスで
  **1bit 配線**（CLK=GPIO13 / CMD=GPIO12 / DAT0=GPIO11）。SD 電源=IOE1 PYG14（`bring_up` で投入）
- **スタック**: `sdio`（SD カード初期化）＋ `embedded-fatfs`（FAT32・async）＋ `embedded-partitions`
- **ファイル**: `NOSTOS.CSV`（再起動をまたいで追記・追記ごとに flush＋unmount）
- **形式**: `time_unix,seq,fix,home,lat_e7,lon_e7,rssi,snr`（緯度経度は 1e7 整数＝PC 側で ÷1e7）
- **異常時**: カード無し/初期化失敗でも受信は継続（起動ログ `sdlog card ok` / `none/fail`、
  追記失敗時のみ `nostos-rx: sdlog append failed`）
- 実装: [`src/sdlog.rs`]。カードは要 FAT32 フォーマット。SD 初期化は起動時 1 回のため、
  **カード挿入後は設定画面の「REBOOT」行タップでウォームリセット**して再初期化する
  （電源ボタンや PC 不要・下記）。
- **カードの抜き方（2026-09-16）**: 設定画面の **「SD CARD」行をタップ**（`LOGGING` → `REMOVE OK`）。
  ステータスログに `sd_eject` 行を書いてからロガーを破棄し、以後は受信/ステータスとも SD に
  書かないので安全に抜ける。再使用はカードを挿して REBOOT 行。起動時にカード無しなら `NO CARD`。
  ⚠️ REBOOT タップ直後に抜くのは**逆に危険**（再起動直後に SD 初期化＋`boot` 行の書き込みが走る）。
- **設定画面「REBOOT（WARM）」行**: タップで `esp_hal::system::software_reset()`（ソフトリセット）。
  電源レール保持のままファーム再実行＝**パネル固着なしで再起動**し microSD を再初期化する。
  電源ボタン（4 秒長押しはコールドサイクル→固着）を使わずに済むフィールド運用向けの再起動手段。

### ステータスログ `STATUS.CSV`（2026-09-16 実機確認）

受信の有無に関わらず、機体の電源・無線・受信経過を **10 分ごと＋事象発生時**に同じ SD へ追記する
ヘルスログ。バッテリ枯渇までの放電カーブや、コールドブート試験のリセット理由・パネル初期化結果を
事後に追う目的（2026-09-16 の「2 日放置で電池枯渇→パネル固着」を機に追加）。

- **周期**: `STATUS_LOG_SECS`（600 秒）。事象行を書いた時点で周期タイマは打ち直す
- **事象**（`event` 列）: `boot`（起動直後）／`boot_panel_fail`（パネル初期化失敗で起動）／
  `periodic`／`low_batt`（VBAT が `led::LOW_BATT_MV`=3500mV を下回った立ち上がり 1 回）／
  `rx_lost`（最終受信から `STALE_REDRAW_SECS`=180 秒経過の立ち上がり 1 回・次の受信で再武装）／
  `sd_eject`（設定画面「SD CARD」行でロガー停止。SD 上の最終行になる）
- **列**: `uptime_s,est_unix,vbat_mv,vin_mv,batt_pct,pwr_src,pwr_cfg,rssi_floor,sx_status,`
  `last_rx_age_s,rx_count,frontlight,reset_reason,event`
  - `est_unix`: 壁時計が無いため「最終受信フレームの GPS 時刻＋経過秒」の推定。未受信は 0
    （放置中は `uptime_s` が主キー）
  - `pwr_src`（PM1 0x04）: bit0=5VIN／bit1=5VINOUT／bit2=電池。`pwr_cfg`（PM1 0x06）: bit0=充電有効／
    bit1=DCDC／bit2=LDO／bit3=BOOST／bit4=LED
  - `rssi_floor`/`sx_status`: SX1262 の瞬時 RSSI[dBm]／ステータス生値。`reset_reason`: ESP32-S3 ROM の
    リセット理由（0x01=電源投入／0x03=ソフト／0x0C=CPU ソフト／0x0F=ブラウンアウト／0x15=USB-UART／
    0x16=USB-JTAG）
  - 読めなかった値は `--`
- **シリアルにも同じ行**を `nostos-status: ...` として出力する（SD 無しでも観測可）。追記失敗は
  `nostos-fw: status sdlog append failed`
- 実装: [`src/statuslog.rs`]（行の組み立て・リセット理由）、[`src/main.rs`] `log_status()`（5 秒の
  電源計測ティックで判定）、[`src/sdlog.rs`] `append_status()`（`NOSTOS.CSV` と同じマウント/アンマウント
  方式）。実機起動行の例:
  `6,0,3926,4980,69,0x05,0x07,-109,0xd2,--,0,0,0x15,boot`

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
