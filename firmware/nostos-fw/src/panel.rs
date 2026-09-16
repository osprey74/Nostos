//! SSD1677 e-paper 制御（M5GFX 工場駆動方式／OTP 波形の 2 経路・モノクロ＋4 階調）。
//!
//! papermono-rs `firmware/embassy-debug` の `panel.rs`（MIT）から移植
//! （テレメトリ・ターゲットマークは除外）。モノクロ（軌跡画面・部分更新可）と
//! 4 階調（帰路画面・薄墨表現）の両経路を持つ。
//!
//! # 駆動方式（[`DRIVE`] で切替・2026-09-16）
//! - [`Drive::M5gfx`]（既定）: 工場 M5GFX `Panel_SSD1677_4Gray` と同じ **Mode 2 駆動**。
//!   全面更新は `lut_fast`（48 フレーム・4 階調絶対更新）、モノクロ部分更新は `lut_fastest`
//!   （差分・白黒）。いずれも **カスタム LUT(0x32)＋明示駆動電圧（VGH 0x03 / VSH1・VSH2・VSL
//!   0x04 / VCOM 0x2C）**を書いてから `0x22=0xCC`（クロック＋アナログ ON＋Mode 2 表示）で
//!   起動する。工場 UserDemo は `epd_fast`/`epd_fastest` のみ使用（Mode 1 は未使用）。
//!   コールドブート固着（OTP 内蔵電圧では冷えたパネルの駆動が立たない疑い）への対策。
//! - [`Drive::Otp`]: 従来の工場 OTP 波形（0xF8/0x14 モノ・0xFF 部分・0xD7 4 階調）。
//!
//! # 安全契約（厳守）
//! 1. **LUT は工場 M5GFX の値をバイト単位でそのまま使う**（独自波形は焼き付きの恐れがあるため
//!    禁止。`lut_fast` / `lut_fastest` は M5GFX `Panel_SSD1677.cpp` からの転記）。
//! 2. **部分更新は [`PARTIALS_BEFORE_FULL`] 回で強制フル更新**（DC バイアス蓄積の除去）。
//! 3. **更新後は必ずアナログ OFF（0x22=0x03）→ Deep Sleep Mode 1**（高圧チャージポンプ停止）。
//! 4. **BUSY(GPIO18) が LOW に戻るまで次のトランザクションを出さない**。

use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::{Input, Level, Output, OutputConfig};
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;
use esp_hal::Blocking;
use m5stack_papermono_lite::display;
use m5stack_papermono_lite::ioe1;
use m5stack_papermono_lite::ssd1677_otp::Ssd1677;
use static_cell::ConstStaticCell;

use crate::ioe::{self, SysI2c};

type Epd = Ssd1677<Spi<'static, Blocking>, Output<'static>, Output<'static>>;

/// パネル駆動方式。
#[allow(dead_code)] // 非選択側のバリアントは A/B 比較用に残す
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Drive {
    /// 工場 OTP 波形（内蔵電圧）。
    Otp,
    /// 工場 M5GFX 方式（カスタム LUT＋明示電圧・Mode 2）。
    M5gfx,
}

/// 使用する駆動方式。A/B 比較用に定数で切替。
pub const DRIVE: Drive = Drive::M5gfx;

/// ハードウェアリセットパルス幅 [ms]。
const RST_MS: u64 = 10;

/// Master Activation 後に BUSY の立ち上がりを待つ最大時間 [ms]。
const BUSY_RISE_MS: u64 = 100;

/// ブロッキング RAM 転送中に executor へ譲る行間隔。
const YIELD_EVERY_ROWS: u16 = 16;

/// フル更新を強制するまでの部分更新回数（原本と同じ 18）。
const PARTIALS_BEFORE_FULL: u8 = 18;

// --- M5GFX 方式のコマンド（Panel_SSD1677.cpp） ---
/// Write LUT register（105 バイト）。
const CMD_WRITE_LUT: u8 = 0x32;
/// Gate driving voltage（VGH）。
const CMD_GATE_VOLT: u8 = 0x03;
/// Source driving voltage（VSH1 / VSH2 / VSL）。
const CMD_SOURCE_VOLT: u8 = 0x04;
/// Write VCOM register。
const CMD_WRITE_VCOM: u8 = 0x2C;
/// 0x22: Mode 2 表示（0x0C）＋クロック・アナログ投入（0xC0）。リセット直後は電源 OFF 状態
/// なので M5GFX `_activate` と同じく毎回 0xC0 を立てる。
const CTRL2_MODE2_POWER_ON_DISPLAY: u8 = 0xCC;
/// 0x22: アナログ OFF＋クロック OFF（M5GFX `setSleep(true)` / `setPowerSave(true)`）。
const CTRL2_POWER_OFF: u8 = 0x03;

/// M5GFX `lut_fast`: Mode 2・48 フレームの 4 階調絶対更新（工場 `epd_fast`）。
/// 先頭 105 バイトが 0x32、末尾 5 バイトが VGH / VSH1(+15V) / VSH2 / VSL(-15V) / VCOM(-1.2V)。
/// RAM グループ（RED<<1 | BW）: 0=白 / 1=薄墨 / 2=濃墨 / 3=黒。
static LUT_FAST: [u8; 110] = [
    0x55, 0x55, 0x55, 0x55, 0x55, 0x5A, 0xAA, 0xAA, 0x00, 0x00, // white
    0xAA, 0x95, 0x55, 0x55, 0x55, 0x5A, 0x82, 0xAC, 0x00, 0x00, // light
    0xAA, 0xA5, 0x55, 0x55, 0x55, 0x5A, 0xAC, 0x00, 0x00, 0x00, // dark
    0xAA, 0xAA, 0xAA, 0xAA, 0x55, 0x55, 0x55, 0x50, 0x00, 0x00, // black
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // VCOM
    0x01, 0x01, 0x01, 0x01, 0x00, //
    0x01, 0x01, 0x01, 0x01, 0x00, //
    0x01, 0x01, 0x01, 0x01, 0x00, //
    0x01, 0x01, 0x01, 0x01, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x8F, 0x8F, 0x8F, 0x8F, 0x8F, // frame rate
    0x17, 0x41, 0xA8, 0x32, 0x30, // VGH, VSH1, VSH2, VSL, VCOM
];

/// M5GFX `lut_fastest`: Mode 2・差分モノクロ更新（工場 `epd_fastest`）。
/// RAM グループ（RED=旧<<1 | BW=新・1=白）: 0=黒保持 / 1=黒→白 / 2=白→黒 / 3=白保持。
/// 2 フレームの逆極性プリパルス＋8 フレーム駒。
static LUT_FASTEST: [u8; 110] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // hold dark
    0x6A, 0xA0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // black -> white
    0x95, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // white -> black
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // hold light
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // VCOM
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x01, 0x01, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x8F, 0x8F, 0x8F, 0x8F, 0x8F, // frame rate
    0x17, 0x46, 0xA8, 0x32, 0x30, // VGH, VSH1(+16V), VSH2, VSL(-15V), VCOM(-1.2V)
];

/// 差分更新の基準（ガラス上に出ているモノクロ画像の黒マスク・ページ座標 1bpp）。
static DISPLAYED_PLANE: ConstStaticCell<[u8; display::PLANE_BYTES]> =
    ConstStaticCell::new([0; display::PLANE_BYTES]);

/// e-paper パネルハンドル（部分更新バジェット管理付き）。
pub struct Panel {
    epd: Epd,
    /// コントローラ RAM に有効なモノクロベースラインが書かれているか（OTP 経路）。
    mono_ready: bool,
    /// 前回フル更新からの部分更新回数。
    partials: u8,
    /// ガラス上のモノクロ画像（黒=1）。M5GFX 差分更新の旧フレーム。
    displayed: &'static mut [u8; display::PLANE_BYTES],
    /// `displayed` がガラス上の画像と一致しているか（4 階調描画後は false）。
    displayed_valid: bool,
    /// 直近の Master Activation で BUSY が立ち上がったか（None=未描画）。ステータスログ用。
    last_busy_rose: Option<bool>,
}

/// SSD1677 を初期化しパネルを立ち上げる（EPD_VDD 投入→リセット→SW_RESET）。
pub async fn begin(
    i2c: &mut SysI2c,
    spi2: esp_hal::peripherals::SPI2<'static>,
    mosi: esp_hal::gpio::AnyPin<'static>,
    sclk: esp_hal::gpio::AnyPin<'static>,
    cs: esp_hal::gpio::AnyPin<'static>,
    dc: esp_hal::gpio::AnyPin<'static>,
    busy: &Input<'static>,
) -> Option<Panel> {
    // 【2026-09-14】EPD_VDD レールを一度 LOW に落として放電させ、コールド状態から立ち上げ直す。
    // 単独では固着を解かなかったが、工場の電源ボタン再起動（IOE1 ごと失電）と同じ順序への
    // 是正として維持する。放電中は RST を LOW 保持。
    // ⚠️ EPD_VDD_EN(io3) は IN 読み戻しで駒動を確認する（コールド起動直後の IOE1 は登録どおりに
    // 出力ドライバが有効化されないことがあり、パネル無電源のまま初期化が空振りする＝
    // コールドブート固着の真因。2026-09-16 実機で特定）。
    let _ = ioe::set_output_verified(i2c, ioe1::EPD_RST, false);
    ioe::set_output_verified(i2c, ioe1::EPD_VDD_ENABLE, false);
    Timer::after(Duration::from_millis(300)).await; // レール放電待ち
    if !ioe::set_output_verified(i2c, ioe1::EPD_VDD_ENABLE, true) {
        esp_println::println!("nostos-fw: panel EPD_VDD_EN not driven -> begin FAILED");
        return None;
    }
    // コールドスタート時はレールが 0V から立ち上がるため、リセット前に十分待つ。
    Timer::after(Duration::from_millis(500)).await;
    ioe::dump_ioe_gpio(i2c, "pre-rst");
    let busy_before_rst = busy.is_high();
    let _ = ioe::set_push_pull_output(i2c, ioe1::EPD_RST, false);
    Timer::after(Duration::from_millis(RST_MS)).await;
    let busy_in_rst = busy.is_high();
    let _ = ioe::set_output_verified(i2c, ioe1::EPD_RST, true);
    let t0 = Instant::now();
    Timer::after(Duration::from_millis(1)).await;
    let busy_after_rst = busy.is_high();
    Timer::after(Duration::from_millis(RST_MS)).await;
    wait_busy_low(busy).await;
    esp_println::println!(
        "nostos-fw: panel diag hw_rst busy before={} in_rst={} after1ms={} low_after={}ms",
        busy_before_rst as u8,
        busy_in_rst as u8,
        busy_after_rst as u8,
        t0.elapsed().as_millis()
    );
    ioe::dump_ioe_gpio(i2c, "post-rst");

    let Ok(spi) = Spi::new(
        spi2,
        Config::default().with_frequency(Rate::from_hz(display::OTP_SPI_HZ)),
    ) else {
        return None;
    };
    let spi = spi.with_mosi(mosi).with_sck(sclk);
    let cs = Output::new(cs, Level::High, OutputConfig::default());
    let dc = Output::new(dc, Level::Low, OutputConfig::default());
    let mut epd = Ssd1677::new(spi, dc, cs);

    // 診断: SW リセットと RAM 自動クリアで BUSY が立ち上がるか（= SPI コマンドが
    // コントローラに届き処理されているか）。コールドブート固着では Master Activation で
    // BUSY が上がらないことが判明（2026-09-16）。ここでどの段階から無反応かを切り分ける。
    let t0 = Instant::now();
    let _ = epd.cmd(display::SW_RESET, &[]);
    Timer::after(Duration::from_millis(1)).await;
    let sw_busy_1ms = busy.is_high();
    Timer::after(Duration::from_millis(RST_MS)).await;
    wait_busy_low(busy).await;
    esp_println::println!(
        "nostos-fw: panel diag sw_rst busy_after1ms={} low_after={}ms",
        sw_busy_1ms as u8,
        t0.elapsed().as_millis()
    );

    // 工場ドライバ（M5GFX `_after_wake`）準拠のフル初期化＋RAM 自動クリア。
    let _ = epd.init_mono();
    wait_busy_low(busy).await;
    let t0 = Instant::now();
    let _ = epd.cmd(0x46, &[0xF7]); // BW RAM 自動クリア（白）
    let rose46 = wait_busy_cycle(busy).await;
    esp_println::println!(
        "nostos-fw: panel diag auto_clear46 busy_rose={} took={}ms",
        rose46 as u8,
        t0.elapsed().as_millis()
    );
    let _ = epd.cmd(0x47, &[0xF7]); // RED RAM 自動クリア（白）
    wait_busy_low(busy).await;

    Some(Panel {
        epd,
        mono_ready: false,
        partials: 0,
        displayed: DISPLAYED_PLANE.take(),
        displayed_valid: false,
        last_busy_rose: None,
    })
}

impl Panel {
    /// 直近の表示更新で BUSY が立ち上がったか（false なら波形が走っていない＝固着の疑い）。
    pub fn last_busy_rose(&self) -> Option<bool> {
        self.last_busy_rose
    }

    /// モノクロフレームを描画する（黒 = `bw` または `red` のビットが立った画素）。
    ///
    /// ベースライン未確立か部分更新バジェット超過ならフル更新、それ以外は
    /// 部分（差分）更新。
    pub async fn paint_mono_fast(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'static>,
    ) {
        let budget_exhausted = self.partials >= PARTIALS_BEFORE_FULL;
        match DRIVE {
            Drive::M5gfx => {
                if !self.displayed_valid || budget_exhausted {
                    self.m5_mono_absolute(i2c, bw, red, busy).await;
                } else {
                    self.m5_mono_fastest(i2c, bw, red, busy).await;
                }
            }
            Drive::Otp => {
                if !self.mono_ready || budget_exhausted {
                    self.otp_refresh_mono_full(i2c, bw, red, busy).await;
                } else {
                    self.otp_refresh_partial(i2c, bw, red, busy).await;
                }
            }
        }
    }

    /// 4 階調フレームを描画する（常にフル更新）。
    ///
    /// プレーンのビットは `ssd1677-otp::gray_planes` のエンコード
    /// （WHITE=(0,0) / LIGHT=(bw) / DARK=(red) / BLACK=(1,1)）。M5GFX の RAM エンコード
    /// （`~v`：BW=~(v&1) / RED=~(v&2)）と一致するのでそのまま両 RAM に書く。
    /// 4 階調描画はモノクロ基準を無効化するため、次のモノクロ描画は自動的にフル更新になる。
    pub async fn paint_gray(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'static>,
    ) {
        match DRIVE {
            Drive::M5gfx => self.m5_gray_absolute(i2c, bw, red, busy).await,
            Drive::Otp => self.otp_paint_gray(i2c, bw, red, busy).await,
        }
    }

    // -----------------------------------------------------------------------
    // M5GFX 方式（Mode 2・カスタム LUT＋明示電圧）
    // -----------------------------------------------------------------------

    /// Deep Sleep から復帰させて工場 `_after_wake` 相当の初期化を行う
    /// （HW リセット→SW リセット→list0＋アドレッシング→RAM 自動クリア）。
    async fn m5_wake(&mut self, i2c: &mut SysI2c, busy: &Input<'_>) {
        self.hardware_reset(i2c, busy).await;
        let _ = self.epd.cmd(display::SW_RESET, &[]);
        Timer::after(Duration::from_millis(RST_MS)).await;
        wait_busy_low(busy).await;
        let _ = self.epd.init_mono();
        wait_busy_low(busy).await;
        let _ = self.epd.cmd(0x46, &[0xF7]);
        wait_busy_low(busy).await;
        let _ = self.epd.cmd(0x47, &[0xF7]);
        wait_busy_low(busy).await;
    }

    /// 110 バイト LUT を書く（M5GFX `send_lut`）: 105 バイト→0x32、電圧 5 バイト→0x03/0x04/0x2C。
    fn m5_send_lut(&mut self, lut: &[u8; 110]) {
        let _ = self.epd.cmd(CMD_WRITE_LUT, &lut[..105]);
        let _ = self.epd.cmd(CMD_GATE_VOLT, &[lut[105]]);
        let _ = self.epd.cmd(CMD_SOURCE_VOLT, &lut[106..109]);
        let _ = self.epd.cmd(CMD_WRITE_VCOM, &[lut[109]]);
    }

    /// Mode 2 で表示更新を起動し完了を待つ（M5GFX `_activate(CTRL1_NORMAL, 0x0C)`＋電源投入）。
    /// 診断として BUSY の立ち上がり有無と所要時間をシリアルへ出す（コールドブート固着では
    /// BUSY が上がらない／即戻る＝波形が走っていない、を切り分けるため）。
    async fn m5_activate(&mut self, what: &str, busy: &Input<'_>) -> bool {
        let _ = self.epd.cmd(
            display::DISPLAY_UPDATE_CONTROL_1,
            &[display::DISPLAY_CTRL1_NORMAL],
        );
        let _ = self.epd.cmd(
            display::DISPLAY_UPDATE_CONTROL_2,
            &[CTRL2_MODE2_POWER_ON_DISPLAY],
        );
        let t0 = Instant::now();
        let _ = self.epd.activate();
        let rose = wait_busy_cycle(busy).await;
        self.last_busy_rose = Some(rose);
        esp_println::println!(
            "nostos-fw: panel m5 {} busy_rose={} took={}ms",
            what,
            rose as u8,
            t0.elapsed().as_millis()
        );
        rose
    }

    /// アナログ・クロック OFF → Deep Sleep Mode 1（M5GFX `setSleep(true)`）。
    async fn m5_power_off_sleep(&mut self, busy: &Input<'_>) {
        let _ = self
            .epd
            .cmd(display::DISPLAY_UPDATE_CONTROL_2, &[CTRL2_POWER_OFF]);
        let _ = self.epd.activate();
        wait_busy_low(busy).await;
        self.deep_sleep().await;
    }

    /// `displayed` を現在のモノクロ黒マスクで更新する。
    fn m5_remember_mono(&mut self, bw: &[u8], red: &[u8]) {
        for (i, d) in self.displayed.iter_mut().enumerate() {
            *d = bw.get(i).copied().unwrap_or(0) | red.get(i).copied().unwrap_or(0);
        }
        self.displayed_valid = true;
    }

    /// モノクロ全面（Mode 2 絶対更新・`lut_fast`）。黒 → 両 RAM=1（グループ 3）、白 → 0。
    async fn m5_mono_absolute(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'_>,
    ) {
        self.m5_wake(i2c, busy).await;
        write_plane(&mut self.epd, display::WRITE_RAM_BW, |x, y| {
            official_bit(bw, x, y) || official_bit(red, x, y)
        })
        .await;
        write_plane(&mut self.epd, display::WRITE_RAM_RED, |x, y| {
            official_bit(bw, x, y) || official_bit(red, x, y)
        })
        .await;
        self.m5_send_lut(&LUT_FAST);
        let _ = self.m5_activate("mono_abs", busy).await;
        self.m5_power_off_sleep(busy).await;
        self.m5_remember_mono(bw, red);
        self.partials = 0;
    }

    /// モノクロ差分（Mode 2・`lut_fastest`）。BW RAM=新（1=白）、RED RAM=旧（1=白）。
    async fn m5_mono_fastest(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'_>,
    ) {
        self.m5_wake(i2c, busy).await;
        write_plane(&mut self.epd, display::WRITE_RAM_BW, |x, y| {
            !(official_bit(bw, x, y) || official_bit(red, x, y))
        })
        .await;
        {
            let old: &[u8] = &self.displayed[..];
            // 借用衝突回避のため旧フレームをローカル参照に束ねる（epd と displayed は別フィールド）。
            let epd = &mut self.epd;
            write_plane(epd, display::WRITE_RAM_RED, |x, y| !official_bit(old, x, y)).await;
        }
        self.m5_send_lut(&LUT_FASTEST);
        let _ = self.m5_activate("mono_diff", busy).await;
        self.m5_power_off_sleep(busy).await;
        self.m5_remember_mono(bw, red);
        self.partials = self.partials.saturating_add(1);
    }

    /// 4 階調全面（Mode 2 絶対更新・`lut_fast`）。プレーンはそのまま両 RAM へ。
    async fn m5_gray_absolute(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'_>,
    ) {
        self.m5_wake(i2c, busy).await;
        write_plane(&mut self.epd, display::WRITE_RAM_BW, |x, y| {
            official_bit(bw, x, y)
        })
        .await;
        write_plane(&mut self.epd, display::WRITE_RAM_RED, |x, y| {
            official_bit(red, x, y)
        })
        .await;
        self.m5_send_lut(&LUT_FAST);
        let _ = self.m5_activate("gray_abs", busy).await;
        self.m5_power_off_sleep(busy).await;
        // ガラス上は 4 階調画像 → モノクロ差分の基準として使えない。
        self.displayed_valid = false;
        self.mono_ready = false;
        self.partials = 0;
    }

    // -----------------------------------------------------------------------
    // OTP 方式（従来）
    // -----------------------------------------------------------------------

    async fn otp_paint_gray(&mut self, i2c: &mut SysI2c, bw: &[u8], red: &[u8], busy: &Input<'_>) {
        self.hardware_reset(i2c, busy).await;
        self.otp_init_gray(busy).await;
        write_gray_plane(&mut self.epd, display::WRITE_RAM_BW, bw).await;
        write_gray_plane(&mut self.epd, display::WRITE_RAM_RED, red).await;
        let _ = self.epd.cmd(
            display::DISPLAY_UPDATE_CONTROL_2,
            &[display::UPDATE_SEQ_OTP_4GRAY],
        );
        let _ = self.epd.activate();
        let _ = wait_busy_cycle(busy).await;
        self.deep_sleep().await;
        self.mono_ready = false;
        self.displayed_valid = false;
        self.partials = 0;
    }

    async fn otp_init_gray(&mut self, busy: &Input<'_>) {
        wait_ready(busy).await;
        let _ = self.epd.cmd(display::SW_RESET, &[]);
        Timer::after(Duration::from_millis(RST_MS)).await;
        wait_busy_low(busy).await;
        let _ = self.epd.init_gray();
    }

    async fn otp_init_mono(&mut self, busy: &Input<'_>) {
        wait_ready(busy).await;
        let _ = self.epd.cmd(display::SW_RESET, &[]);
        Timer::after(Duration::from_millis(RST_MS)).await;
        wait_busy_low(busy).await;
        let _ = self.epd.init_mono();
    }

    async fn hardware_reset(&mut self, i2c: &mut SysI2c, busy: &Input<'_>) {
        let _ = ioe::set_push_pull_output(i2c, ioe1::EPD_RST, false);
        Timer::after(Duration::from_millis(RST_MS)).await;
        let _ = ioe::set_push_pull_output(i2c, ioe1::EPD_RST, true);
        Timer::after(Duration::from_millis(RST_MS)).await;
        wait_busy_low(busy).await;
    }

    async fn deep_sleep(&mut self) {
        let _ = self.epd.deep_sleep_mode1();
        Timer::after(Duration::from_millis(display::OTP_SLEEP_MS)).await;
    }

    async fn otp_wake_for_partial(&mut self, i2c: &mut SysI2c, busy: &Input<'_>) {
        self.hardware_reset(i2c, busy).await;
        let _ = self.epd.apply_mono_addressing();
        let _ = self
            .epd
            .cmd(display::BORDER_WAVEFORM, &[display::BORDER_OTP_PARTIAL]);
    }

    async fn otp_refresh_mono_full(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'_>,
    ) {
        self.hardware_reset(i2c, busy).await;
        self.otp_init_mono(busy).await;
        let _ = self.epd.cmd(
            display::DISPLAY_UPDATE_CONTROL_2,
            &[display::UPDATE_SEQ_OTP_MONO_SYNC],
        );
        write_official_mono(&mut self.epd, display::WRITE_RAM_BW, bw, red, true).await;
        let _ = self.epd.activate();
        let _ = wait_busy_cycle(busy).await;

        write_official_mono(&mut self.epd, display::WRITE_RAM_RED, bw, red, false).await;
        write_official_mono(&mut self.epd, display::WRITE_RAM_BW, bw, red, false).await;
        let _ = self
            .epd
            .cmd(display::BORDER_WAVEFORM, &[display::BORDER_OTP_FULL]);
        let _ = self.epd.cmd(
            display::DISPLAY_UPDATE_CONTROL_2,
            &[display::UPDATE_SEQ_OTP_MONO],
        );
        let _ = self.epd.activate();
        let _ = wait_busy_cycle(busy).await;
        self.deep_sleep().await;
        self.mono_ready = true;
        self.partials = 0;
    }

    async fn otp_refresh_partial(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'_>,
    ) {
        self.otp_wake_for_partial(i2c, busy).await;
        write_official_mono(&mut self.epd, display::WRITE_RAM_BW, bw, red, false).await;
        let _ = self.epd.cmd(
            display::DISPLAY_UPDATE_CONTROL_1,
            &[display::DISPLAY_CTRL1_NORMAL],
        );
        let _ = self.epd.cmd(
            display::DISPLAY_UPDATE_CONTROL_2,
            &[display::UPDATE_SEQ_OTP_PARTIAL],
        );
        let _ = self.epd.activate();
        let _ = wait_busy_cycle(busy).await;
        write_official_mono(&mut self.epd, display::WRITE_RAM_RED, bw, red, false).await;
        self.deep_sleep().await;
        self.partials = self.partials.saturating_add(1);
    }
}

/// 1 プレーンを RAM へ書く（X/Y インクリメント・原点から）。`bit(px, py)` が true の画素を 1 に。
/// ページ座標（480×800 公式向き）→ OTP RAM（800×480）の写像は `otp_ram_to_usb_down`。
async fn write_plane<F: Fn(u16, u16) -> bool>(epd: &mut Epd, ram_cmd: u8, bit: F) {
    let _ = epd.rewind();
    let _ = epd.begin_ram(ram_cmd);
    let mut row = [0u8; display::OTP_BYTES_PER_ROW];
    for ram_y in 0..display::OTP_RAM_HEIGHT {
        row.fill(0x00);
        for ram_x in 0..display::OTP_RAM_WIDTH {
            let (px, py) = display::otp_ram_to_usb_down(ram_x, ram_y);
            if bit(px, py) {
                let byte = (ram_x / 8) as usize;
                let b = (7 - (ram_x % 8)) as u8;
                row[byte] |= 1u8 << b;
            }
        }
        let _ = epd.write_bytes(&row);
        if ram_y.is_multiple_of(YIELD_EVERY_ROWS) {
            Timer::after(Duration::from_millis(1)).await;
        }
    }
    let _ = epd.end_ram();
}

async fn write_gray_plane(epd: &mut Epd, ram_cmd: u8, official: &[u8]) {
    let _ = epd.rewind_gray();
    let _ = epd.begin_ram(ram_cmd);
    let mut row = [0u8; display::OTP_BYTES_PER_ROW];
    for ram_y in 0..display::OTP_RAM_HEIGHT {
        for (byte_i, slot) in row.iter_mut().enumerate() {
            let mut v = 0u8;
            for bit in 0..8u16 {
                let ram_x = display::OTP_RAM_WIDTH
                    .saturating_sub(1)
                    .saturating_sub((byte_i as u16) * 8 + bit);
                let (px, py) = display::otp_ram_to_usb_down(ram_x, ram_y);
                if official_bit(official, px, py) {
                    v |= 0x80 >> bit;
                }
            }
            *slot = v;
        }
        let _ = epd.write_bytes(&row);
        if ram_y.is_multiple_of(YIELD_EVERY_ROWS) {
            Timer::after(Duration::from_millis(1)).await;
        }
    }
    let _ = epd.end_ram();
}

async fn write_official_mono(epd: &mut Epd, ram_cmd: u8, bw: &[u8], red: &[u8], invert: bool) {
    let _ = epd.rewind();
    let _ = epd.begin_ram(ram_cmd);
    let mut row = [0u8; display::OTP_BYTES_PER_ROW];
    for ram_y in 0..display::OTP_RAM_HEIGHT {
        row.fill(0xFF);
        for ram_x in 0..display::OTP_RAM_WIDTH {
            let (px, py) = display::otp_ram_to_usb_down(ram_x, ram_y);
            if official_bit(bw, px, py) || official_bit(red, px, py) {
                ink_black(&mut row, ram_x);
            }
        }
        if invert {
            for b in &mut row {
                *b = !*b;
            }
        }
        let _ = epd.write_bytes(&row);
        if ram_y.is_multiple_of(YIELD_EVERY_ROWS) {
            Timer::after(Duration::from_millis(1)).await;
        }
    }
    let _ = epd.end_ram();
}

fn official_bit(plane: &[u8], x: u16, y: u16) -> bool {
    if x >= display::WIDTH || y >= display::HEIGHT {
        return false;
    }
    let i = usize::from(y) * display::BYTES_PER_ROW + usize::from(x) / 8;
    let mask = 0x80u8 >> (x % 8);
    plane.get(i).is_some_and(|b| b & mask != 0)
}

fn ink_black(row: &mut [u8], ram_x: u16) {
    if ram_x >= display::OTP_RAM_WIDTH {
        return;
    }
    let byte = (ram_x / 8) as usize;
    let bit = (7 - (ram_x % 8)) as u8;
    if byte < row.len() {
        row[byte] &= !((1u8) << bit);
    }
}

async fn wait_ready(busy: &Input<'_>) {
    if busy.is_high() {
        wait_busy_low(busy).await;
    }
}

async fn wait_busy_low(busy: &Input<'_>) {
    Timer::after(Duration::from_millis(1)).await;
    let deadline = Instant::now() + Duration::from_millis(display::OTP_BUSY_TIMEOUT_MS);
    while busy.is_high() && Instant::now() < deadline {
        Timer::after(Duration::from_millis(1)).await;
    }
}

async fn wait_busy_cycle(busy: &Input<'_>) -> bool {
    Timer::after(Duration::from_millis(1)).await;
    let rise_deadline = Instant::now() + Duration::from_millis(BUSY_RISE_MS);
    let mut rose = busy.is_high();
    while !rose && Instant::now() < rise_deadline {
        Timer::after(Duration::from_millis(1)).await;
        rose = busy.is_high();
    }
    wait_busy_low(busy).await;
    rose
}
