//! SSD1677 e-paper 制御（工場 OTP 波形・モノクロ＋4 階調）。
//!
//! papermono-rs `firmware/embassy-debug` の `panel.rs`（MIT）から移植
//! （テレメトリ・ターゲットマークは除外）。モノクロ（軌跡画面・部分更新可）と
//! 4 階調 GrayFull（帰路画面・薄墨表現）の両経路を持つ。
//!
//! # 安全契約（原本と同一・厳守）
//! 1. **工場 OTP 波形のみ使用**（カスタム LUT 0x32 は焼き付きの恐れがあるため禁止）。
//! 2. **部分更新は [`PARTIALS_BEFORE_FULL`] 回で強制フル更新**（DC バイアス蓄積の除去）。
//! 3. **更新後は必ず Deep Sleep Mode 1**（高圧チャージポンプ停止）。
//! 4. **BUSY(GPIO18) が LOW に戻るまで次のトランザクションを出さない**。
//!
//! # コールドブート固着について（2026-09-16 解決）
//! 完全電源断からの起動で e-ink だけ固まる症状の真因は、M5IOE1 の io3（EPD_VDD_ENABLE）が
//! コールド起動直後に登録どおり駒動されず**パネルが無電源**だったこと。波形は無関係。
//! [`begin`] は EPD_VDD / EPD_RST を [`crate::ioe::set_output_verified`]（IN 読み戻し＋MODE 振り直し）で
//! 設定し、起動時に IOE1 レジスタと BUSY の挙動を診断出力する。
//! なお工場 M5GFX 駆動方式（カスタム LUT＋明示電圧）への移植も試したが、黒が薄い／quality は
//! 3.4 秒でタップを取りこぼす／差分は残像、と実機評価が悪く撤回した（git log 2026-09-16）。

use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::{Input, Level, Output, OutputConfig};
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;
use esp_hal::Blocking;
use m5stack_papermono_lite::display;
use m5stack_papermono_lite::ioe1;
use m5stack_papermono_lite::ssd1677_otp::Ssd1677;

use crate::ioe::{self, SysI2c};

type Epd = Ssd1677<Spi<'static, Blocking>, Output<'static>, Output<'static>>;

/// ハードウェアリセットパルス幅 [ms]。
const RST_MS: u64 = 10;

/// Master Activation 後に BUSY の立ち上がりを待つ最大時間 [ms]。
const BUSY_RISE_MS: u64 = 100;

/// ブロッキング RAM 転送中に executor へ譲る行間隔。
const YIELD_EVERY_ROWS: u16 = 16;

/// フル更新を強制するまでの部分更新回数（原本と同じ 18）。
const PARTIALS_BEFORE_FULL: u8 = 18;

/// e-paper パネルハンドル（部分更新バジェット管理付き）。
pub struct Panel {
    epd: Epd,
    /// コントローラ RAM に有効なモノクロベースラインが書かれているか。
    mono_ready: bool,
    /// 前回フル更新からの部分更新回数。
    partials: u8,
    /// 起動時の RAM 自動クリア（0x46）で BUSY が立ち上がったか（false なら波形が走っていない＝
    /// 無電源／固着の疑い）。ステータスログの起動事象に使う。
    boot_busy_rose: bool,
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
    // ⚠️ EPD_VDD_EN(io3) は IN 読み戻しで駒動を確認する（コールド起動直後の IOE1 は登録どおりに
    // 出力ドライバが有効化されないことがあり、パネル無電源のまま初期化が空振りする＝
    // コールドブート固着の真因。2026-09-16 実機で特定）。
    // レールを一度 LOW に落として放電させ、コールド状態から立ち上げ直す。放電中は RST を LOW 保持。
    let _ = ioe::set_output_verified(i2c, ioe1::EPD_RST, false);
    ioe::set_output_verified(i2c, ioe1::EPD_VDD_ENABLE, false);
    Timer::after(Duration::from_millis(300)).await; // レール放電待ち
    if !ioe::set_output_verified(i2c, ioe1::EPD_VDD_ENABLE, true) {
        esp_println::println!("nostos-fw: panel EPD_VDD_EN not driven -> begin FAILED");
        return None;
    }
    // コールドスタート時はレールが 0V から立ち上がるため、リセット前に十分待つ。
    Timer::after(Duration::from_millis(500)).await;

    // 診断: IOE1 の登録値と実ピンレベル（in が out と一致しない電源系ピンがあれば駒動不良）。
    ioe::dump_ioe_gpio(i2c, "pre-rst");
    let busy_before_rst = busy.is_high();
    let _ = ioe::set_output_verified(i2c, ioe1::EPD_RST, false);
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
    // コントローラに届き処理されているか）。無電源のパネルでは BUSY が一切動かない。
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
        boot_busy_rose: rose46,
    })
}

impl Panel {
    /// 起動時の RAM 自動クリアで BUSY が立ち上がったか（false なら固着の疑い）。
    pub fn boot_busy_rose(&self) -> bool {
        self.boot_busy_rose
    }

    /// モノクロフレームを描画する（黒 = `bw` または `red` のビットが立った画素）。
    ///
    /// ベースライン未確立か部分更新バジェット超過ならフル更新、それ以外は
    /// OTP 部分更新（チラつきなし）。
    pub async fn paint_mono_fast(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'static>,
    ) {
        let budget_exhausted = self.partials >= PARTIALS_BEFORE_FULL;
        if !self.mono_ready || budget_exhausted {
            self.refresh_mono_full(i2c, bw, red, busy).await;
            return;
        }
        self.refresh_partial(i2c, bw, red, busy).await;
    }

    /// 4 階調フレームを描画する（工場 OTP 4-gray 波形・常にフル更新）。
    ///
    /// プレーンのビットは `ssd1677-otp::gray_planes` のエンコード
    /// （WHITE=(0,0) / LIGHT=(bw) / DARK=(red) / BLACK=(1,1)）。
    /// GrayFull はモノクロベースラインを無効化するため、次のモノクロ描画は
    /// 自動的にフル更新になる。
    pub async fn paint_gray(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'static>,
    ) {
        self.hardware_reset(i2c, busy).await;
        self.init_gray(busy).await;
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
        self.partials = 0;
    }

    async fn init_gray(&mut self, busy: &Input<'_>) {
        wait_ready(busy).await;
        let _ = self.epd.cmd(display::SW_RESET, &[]);
        Timer::after(Duration::from_millis(RST_MS)).await;
        wait_busy_low(busy).await;
        let _ = self.epd.init_gray();
    }

    async fn init_mono(&mut self, busy: &Input<'_>) {
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

    async fn wake_for_partial(&mut self, i2c: &mut SysI2c, busy: &Input<'_>) {
        self.hardware_reset(i2c, busy).await;
        let _ = self.epd.apply_mono_addressing();
        let _ = self
            .epd
            .cmd(display::BORDER_WAVEFORM, &[display::BORDER_OTP_PARTIAL]);
    }

    async fn refresh_mono_full(
        &mut self,
        i2c: &mut SysI2c,
        bw: &[u8],
        red: &[u8],
        busy: &Input<'_>,
    ) {
        self.hardware_reset(i2c, busy).await;
        self.init_mono(busy).await;
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

    async fn refresh_partial(&mut self, i2c: &mut SysI2c, bw: &[u8], red: &[u8], busy: &Input<'_>) {
        self.wake_for_partial(i2c, busy).await;
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
