//! SSD1677 e-paper 制御（OTP 波形・モノクロ専用の最小移植）。
//!
//! papermono-rs `firmware/embassy-debug` の `panel.rs`（MIT）からモノクロ描画経路のみを
//! 移植（4 階調・テレメトリ・ターゲットマークは除外。薄墨表現が要る帰路画面で
//! `paint_gray` を追加予定）。
//!
//! # 安全契約（原本と同一・厳守）
//! 1. **工場 OTP 波形のみ使用**（カスタム LUT 0x32 は焼き付きの恐れがあるため禁止）。
//! 2. **部分更新は [`PARTIALS_BEFORE_FULL`] 回で強制フル更新**（DC バイアス蓄積の除去）。
//! 3. **更新後は必ず Deep Sleep Mode 1**（高圧チャージポンプ停止）。
//! 4. **BUSY(GPIO18) が LOW に戻るまで次のトランザクションを出さない**。

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
    if ioe::set_push_pull_output(i2c, ioe1::EPD_VDD_ENABLE, true).is_err() {
        return None;
    }
    Timer::after(Duration::from_millis(RST_MS)).await;
    let _ = ioe::set_push_pull_output(i2c, ioe1::EPD_RST, false);
    Timer::after(Duration::from_millis(RST_MS)).await;
    let _ = ioe::set_push_pull_output(i2c, ioe1::EPD_RST, true);
    Timer::after(Duration::from_millis(RST_MS)).await;
    wait_busy_low(busy).await;

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

    let _ = epd.cmd(display::SW_RESET, &[]);
    Timer::after(Duration::from_millis(RST_MS)).await;
    wait_busy_low(busy).await;

    Some(Panel {
        epd,
        mono_ready: false,
        partials: 0,
    })
}

impl Panel {
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
