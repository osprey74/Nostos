//! Nostos PaperMono 受信ファーム本体。
//!
//! C6L ビーコンの 16 バイト NostosFrame（923.000 MHz / BW125 / SF9 / sync 0x3A）を
//! 連続受信し、ブレッドクラム軌跡を e-ink に描画する。**受信専用（送信なし）**。
//!
//! 構成（papermono-rs `embassy-debug` の bring-up を受信専用に移植・MIT）:
//! - [`ioe`]: システム I2C・M5IOE1・電源レール
//! - [`lora`]: SX1262 受信ドライバ（Nostos チャネル camp）
//! - [`panel`]: SSD1677 OTP モノクロ描画（部分更新バジェット管理）
//! - [`draw`]: 軌跡マップのレンダリング（`docs/UI.md` 第1画面）
//! - [`sdlog`] / [`statuslog`]: microSD への受信ログ（`NOSTOS.CSV`）とステータスログ（`STATUS.CSV`）
//!
//! HOME 仕様（`docs/UI.md` 確定事項）:
//! - `FLAG_HOME` 付きフレームで出発点を設定/更新（**Trail には積まない**）
//! - HOME 座標が変わったら新しい行程として Trail をリセット
//! - HOME 未受信の間は「HOME not received」表示（軌跡プロットは通常どおり）
//!
//! 操作: ボタン A（GPIO2）＝ズームイン / ボタン B（GPIO3）＝ズームアウト /
//! タップ＝軌跡 ⇄ 帰路の画面切替（タブ実装までの暫定） /
//! スワイプ＝軌跡マップのパン（1 スワイプ 1 回・自動追従停止） /
//! 長押し（0.8s）＝再センタリング（自動追従へ復帰）。

#![no_std]
#![no_main]

mod draw;
mod ioe;
mod jpfont;
mod led;
mod lora;
mod panel;
mod sdlog;
mod statuslog;

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use m5stack_papermono_lite::display;
use nostos_frame::NostosFrame;
use nostos_nav::{GeoPoint, Trail};
use static_cell::ConstStaticCell;

// ESP-IDF 第二段ブートローダ用アプリ記述子。
esp_bootloader_esp_idf::esp_app_desc!();

/// ブレッドクラム保持数（リングバッファ・超過で最古から上書き）。
const TRAIL_CAP: usize = 256;

/// 受信途絶とみなす秒数（この経過で画面を再描画し AGE を更新）。
const STALE_REDRAW_SECS: u64 = 180;

/// ステータスログ（`STATUS.CSV`）の定期記録間隔 [秒]。
const STATUS_LOG_SECS: u64 = 600;

/// スワイプと判定する最小移動量 [page px]。これ未満はタップ扱い。
const SWIPE_MIN_PX: i32 = 40;

/// 再センタリングの長押し時間 [ms]。
const LONG_PRESS_MS: u64 = 800;

/// 指離れ確定までの無接触 tick 数（50ms 周期 ×3 ≒ 150ms）。
const TOUCH_RELEASE_TICKS: u8 = 3;

/// 表示中の画面。下部タブのタップで切替。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    /// 第1画面：軌跡マップ（モノクロ・部分更新）。
    Trail,
    /// 第2画面：帰路ナビ（4 階調・常にフル更新）。
    Homing,
    /// 第3画面：設定（モノクロ）。
    Settings,
}

/// フロントライト輝度 5 段階のデューティ（0=OFF〜4=最大）。
fn brightness_duty(idx: usize) -> u16 {
    use m5stack_papermono_lite::pmic::PWM0_DUTY_MAX;
    match idx {
        0 => 0,
        1 => PWM0_DUTY_MAX / 8,
        2 => PWM0_DUTY_MAX / 4,
        3 => PWM0_DUTY_MAX / 2,
        _ => PWM0_DUTY_MAX,
    }
}

/// 自動消灯までの無操作時間 [秒]。
const AUTO_OFF_SECS: u64 = 30;

// e-ink 用 1bpp プレーン（480×800 / 8 = 48,000 バイト ×2）。静的確保。
static BW_PLANE: ConstStaticCell<[u8; display::PLANE_BYTES]> =
    ConstStaticCell::new([0; display::PLANE_BYTES]);
static RED_PLANE: ConstStaticCell<[u8; display::PLANE_BYTES]> =
    ConstStaticCell::new([0; display::PLANE_BYTES]);

#[esp_hal::main]
async fn main(_spawner: Spawner) -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    println!("nostos-fw: boot");
    // 今回起動のリセット理由（0x01=電源投入 / 0x03=ソフト / 0x15=USB-UART / 0x16=USB-JTAG）。
    // ステータスログの `reset_reason` 列に記録し、コールドブート試験の切り分けに使う。
    let reset_reason = statuslog::reset_reason_code();
    println!("nostos-fw: reset_reason=0x{:02x}", reset_reason);

    // 起動ビープ（GPIO42 ブザー）。バッテリ単体ブートの生存確認用：
    // ピッ 1 回＝ESP 起動、bring_up 後のピピッ＝PM1 電源維持まで到達。
    let mut buzzer = Output::new(peripherals.GPIO42, Level::Low, OutputConfig::default());
    let bz_delay = esp_hal::delay::Delay::new();
    beep_blocking(&mut buzzer, &bz_delay, 120);

    // 物理ボタン（active-low・内部プルアップ）。
    let btn_a = Input::new(
        peripherals.GPIO2,
        InputConfig::default().with_pull(Pull::Up),
    );
    let btn_b = Input::new(
        peripherals.GPIO3,
        InputConfig::default().with_pull(Pull::Up),
    );
    // SSD1677 BUSY（データシート準拠プルアップ）。
    let busy = Input::new(
        peripherals.GPIO18,
        InputConfig::default().with_pull(Pull::Up),
    );
    // FT6336G タッチ割り込み（active-low・タップで画面切替）。
    let tp = Input::new(
        peripherals.GPIO4,
        InputConfig::default().with_pull(Pull::Up),
    );

    // システム I2C（GPIO47 SDA / GPIO48 SCL・100 kHz）→ 電源レール bring-up。
    let mut i2c = I2c::new(peripherals.I2C0, I2cConfig::default())
        .expect("I2C0")
        .with_sda(peripherals.GPIO47)
        .with_scl(peripherals.GPIO48);
    let ioe_ok = ioe::bring_up(&mut i2c).await.is_some();
    // PM1 電源維持（LDO_EN/HOLD/WDT 無効）まで到達した合図（ピピッ）。
    beep_blocking(&mut buzzer, &bz_delay, 60);
    bz_delay.delay_millis(70);
    beep_blocking(&mut buzzer, &bz_delay, 60);

    // SSD1677 パネル（SPI2: MOSI=14 / SCLK=15 / CS=16 / DC=17）。
    let mut panel = panel::begin(
        &mut i2c,
        peripherals.SPI2,
        peripherals.GPIO14.into(),
        peripherals.GPIO15.into(),
        peripherals.GPIO16.into(),
        peripherals.GPIO17.into(),
        &busy,
    )
    .await;
    if panel.is_none() {
        println!("nostos-fw: panel begin FAILED (ioe_ok={})", ioe_ok as u8);
    }

    // SX1262（SPI3: MOSI=38 / SCK=39 / MISO=40 / NSS=41、BUSY=21）。
    let nss = Output::new(peripherals.GPIO41, Level::High, OutputConfig::default());
    let sx_busy = Input::new(
        peripherals.GPIO21,
        InputConfig::default().with_pull(Pull::None),
    );
    let spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default().with_frequency(Rate::from_mhz(8)),
    )
    .expect("SPI3")
    .with_sck(peripherals.GPIO39)
    .with_mosi(peripherals.GPIO38)
    .with_miso(peripherals.GPIO40);
    let mut radio = lora::Radio::new(spi, nss, sx_busy);
    radio.power_up(&mut i2c).await;
    let radio_ok = radio.probe();
    println!("nostos-fw: sx1262 probe={}", radio_ok as u8);
    if radio_ok {
        radio.start_rx();
        println!("nostos-fw: rx camp 923.000MHz BW125 SF9 sync 0x3A");
    }

    // microSD CSV ロガー（SDHOST 1bit: CLK=GPIO13 / CMD=GPIO12 / DAT0=GPIO11。
    // SD 電源=IOE1 PYG14 は bring_up で投入済み）。カード無しでも受信は継続する。
    let mut sdlog = sdlog::SdLogger::init(
        peripherals.SDHOST,
        peripherals.GPIO13,
        peripherals.GPIO12,
        peripherals.GPIO11,
    )
    .await;
    println!(
        "nostos-fw: sdlog {}",
        if sdlog.is_some() { "card ok" } else { "none/fail" }
    );

    // アプリ状態。
    let bw = BW_PLANE.take();
    let red = RED_PLANE.take();
    let mut trail: Trail<TRAIL_CAP> = Trail::new();
    let mut home: Option<GeoPoint> = None;
    let mut last: Option<draw::LastRx> = None;
    let mut last_rx_at: Option<Instant> = None;
    let mut last_render_at = Instant::now();
    let mut scale_idx: usize = 1; // 2 m/px（グリッド 1 マス = 160 m）
    let mut screen = Screen::Trail;
    let mut view_center: Option<GeoPoint> = None; // Some = パン中（自動追従停止）
    let mut prev_a = false;
    let mut prev_b = false;
    let mut last_toggle_at = Instant::now();
    // タッチジェスチャ状態。
    let mut touch_active = false;
    let mut touch_start = (0i32, 0i32);
    let mut touch_last = (0i32, 0i32);
    let mut touch_started_at = Instant::now();
    let mut touch_idle: u8 = 0;
    // 設定・電源・LED 状態。
    let mut brightness_idx: usize = 0; // 0=OFF
    let mut auto_off = true;
    let mut light_dimmed = false; // 自動消灯で一時 OFF 中
    let mut last_input_at = Instant::now();
    let mut vbat_cache = ioe::read_vbat_mv(&mut i2c);
    let mut vin_cache = ioe::read_vin_mv(&mut i2c);
    let mut led_ctl = led::Led::new();
    // ステータスログ状態。事象（低電池・途絶）は立ち上がりエッジで 1 回だけ記録する。
    let mut rx_count: u32 = 0;
    let mut last_status_at = Instant::now();
    let mut low_batt_logged = false;
    let mut rx_lost_logged = false;
    // 設定画面「SD CARD」行でロガーを停止した（カードを抜いてよい）か。
    let mut sd_ejected = false;

    // 初期画面（受信待ち）。
    render_and_paint(
        bw,
        red,
        &trail,
        last,
        last_rx_at,
        home,
        scale_idx,
        screen,
        view_center,
        radio_ok,
        (brightness_idx, auto_off, vbat_cache, vin_cache),
        sd_state(&sdlog, sd_ejected),
        &mut panel,
        &mut i2c,
        &busy,
    )
    .await;

    // 起動直後のステータス行（リセット理由・パネル/無線初期化結果・電源状態を残す）。
    log_status(
        &mut sdlog,
        if panel.is_some() { "boot" } else { "boot_panel_fail" },
        &mut i2c,
        &mut radio,
        radio_ok,
        &StatusCtx {
            last,
            last_rx_at,
            vbat_mv: vbat_cache,
            vin_mv: vin_cache,
            rx_count,
            frontlight: 0,
            reset_reason,
        },
    )
    .await;

    let mut dbg_tick: u32 = 0;
    loop {
        Timer::after(Duration::from_millis(50)).await;
        let mut redraw = false;

        // 診断: 60 秒ごとに SX1262 の生状態をダンプ（受信不能時の切り分け用）。
        dbg_tick += 1;
        if dbg_tick % 1200 == 0 {
            let (irq, rssi, raw) = radio.debug_status();
            let touch = ioe::read_touch(&mut i2c);
            println!(
                "nostos-fw: dbg irq=0x{:04x} rssi={} status=0x{:02x} busy={} tp_int={} tp_read={}",
                irq,
                rssi,
                raw,
                busy.is_high() as u8,
                tp.is_high() as u8,
                touch.is_some() as u8
            );
        }
        // 電源状態は 5 秒ごとに更新（LED 判定と設定画面表示に使用）。
        // 同じタイミングでステータスログの記録判定も行う（定期 + 立ち上がりエッジ事象）。
        if dbg_tick % 100 == 0 {
            vbat_cache = ioe::read_vbat_mv(&mut i2c);
            vin_cache = ioe::read_vin_mv(&mut i2c);

            let mut event: Option<&str> = None;
            if last_status_at.elapsed() >= Duration::from_secs(STATUS_LOG_SECS) {
                event = Some("periodic");
            }
            let low = vbat_cache.is_some_and(|v| v > 0 && v < led::LOW_BATT_MV);
            if low && !low_batt_logged {
                low_batt_logged = true;
                event = Some("low_batt");
            } else if !low {
                low_batt_logged = false;
            }
            let lost = last_rx_at.is_some_and(|t| t.elapsed().as_secs() >= STALE_REDRAW_SECS);
            if lost && !rx_lost_logged {
                rx_lost_logged = true;
                event = Some("rx_lost");
            }
            if let Some(ev) = event {
                log_status(
                    &mut sdlog,
                    ev,
                    &mut i2c,
                    &mut radio,
                    radio_ok,
                    &StatusCtx {
                        last,
                        last_rx_at,
                        vbat_mv: vbat_cache,
                        vin_mv: vin_cache,
                        rx_count,
                        frontlight: if light_dimmed { 0 } else { brightness_idx as u8 },
                        reset_reason,
                    },
                )
                .await;
                last_status_at = Instant::now();
            }
        }

        // --- 受信ポーリング ---
        if let Some(pkt) = radio.poll() {
            match NostosFrame::decode(&pkt.buf[..pkt.len]) {
                Ok(f) => {
                    println!(
                        "nostos-rx: seq={} fix={} home={} lat_e7={} lon_e7={} time={} rssi={} snr={} len={}",
                        f.seq,
                        f.has_fix() as u8,
                        f.is_home() as u8,
                        f.lat_e7,
                        f.lon_e7,
                        f.time_unix,
                        pkt.rssi,
                        pkt.snr,
                        pkt.len
                    );
                    if f.is_home() {
                        // 出発点の設定/更新。座標が変わるか、C6L が HOME 再確定（長押し→
                        // seq リセット）した直後の seq=0 フレームなら新しい行程 → Trail リセット。
                        let p = f.geopoint();
                        let changed = f.seq == 0
                            || home.is_none_or(|h| nostos_nav::haversine_m(h, p) > 1.0);
                        if changed {
                            trail = Trail::new();
                            view_center = None;
                            println!("nostos-rx: HOME set, trail reset");
                        }
                        home = Some(p);
                    } else if f.has_fix() {
                        trail.push(f.geopoint());
                        // HOME 未受信の間は最古点への距離・方位を参考出力（rxtest 互換）。
                        let anchor = home.or_else(|| trail.oldest());
                        if let Some(h) = anchor.and_then(|a| trail.homing(a)) {
                            println!(
                                "nostos-rx: trail={} home_set={} home_dist_m={} home_bearing_deg={}",
                                trail.len(),
                                home.is_some() as u8,
                                h.distance_m as i32,
                                h.bearing_deg as i32
                            );
                        }
                    }
                    last = Some(draw::LastRx {
                        seq: f.seq,
                        fix: f.has_fix(),
                        lat_e7: f.lat_e7,
                        lon_e7: f.lon_e7,
                        time_unix: f.time_unix,
                        rssi: pkt.rssi,
                        snr: pkt.snr,
                    });
                    last_rx_at = Some(Instant::now());
                    rx_count = rx_count.saturating_add(1);
                    rx_lost_logged = false;
                    redraw = true;

                    // microSD へ CSV 追記（time_unix,seq,fix,home,lat_e7,lon_e7,rssi,snr）。
                    if let Some(logger) = sdlog.as_mut() {
                        use core::fmt::Write as _;
                        let mut lb = LineBuf::new();
                        let _ = write!(
                            lb,
                            "{},{},{},{},{},{},{},{}\n",
                            f.time_unix,
                            f.seq,
                            f.has_fix() as u8,
                            f.is_home() as u8,
                            f.lat_e7,
                            f.lon_e7,
                            pkt.rssi,
                            pkt.snr
                        );
                        if !logger.append(lb.as_bytes()).await {
                            println!("nostos-rx: sdlog append failed");
                        }
                    }
                }
                Err(e) => {
                    let n = pkt.len.min(8);
                    println!(
                        "nostos-rx: decode err {:?} len={} head={:02x?} rssi={}",
                        e,
                        pkt.len,
                        &pkt.buf[..n],
                        pkt.rssi
                    );
                }
            }
        }

        // --- ボタン（立ち下がりエッジ。地図＝ズーム / 設定＝明るさ）---
        let a = btn_a.is_low();
        let b = btn_b.is_low();
        let mut input = false;
        if a && !prev_a {
            input = true;
            if screen == Screen::Settings {
                if brightness_idx < 4 {
                    brightness_idx += 1;
                    ioe::set_frontlight(&mut i2c, brightness_duty(brightness_idx));
                    redraw = true;
                }
            } else if scale_idx > 0 {
                scale_idx -= 1;
                redraw = true;
            }
        }
        if b && !prev_b {
            input = true;
            if screen == Screen::Settings {
                if brightness_idx > 0 {
                    brightness_idx -= 1;
                    ioe::set_frontlight(&mut i2c, brightness_duty(brightness_idx));
                    redraw = true;
                }
            } else if scale_idx + 1 < draw::SCALE_M_PER_PX.len() {
                scale_idx += 1;
                redraw = true;
            }
        }
        prev_a = a;
        prev_b = b;

        // --- タッチジェスチャ判別 ---
        // タップ（短・移動小）＝画面切替 / スワイプ（移動大）＝軌跡マップのパン /
        // 長押し（0.8s・移動小）＝再センタリング。指を離した時点で 1 回だけ判定する
        // （e-ink は追従描画不可のため）。ホールド中は INT が上がっても座標を読み続ける。
        let mut pt: Option<(i32, i32)> = None;
        if tp.is_low() || touch_active {
            if let Some((n, fx, fy)) = ioe::read_touch(&mut i2c) {
                if n >= 1 {
                    if let Some((px, py)) =
                        display::framebuffer_to_page(fx, fy, display::PageRotation::Portrait0)
                    {
                        pt = Some((i32::from(px), i32::from(py)));
                    }
                }
            }
        }
        match pt {
            Some(p) => {
                if !touch_active {
                    touch_active = true;
                    touch_start = p;
                    touch_started_at = Instant::now();
                }
                touch_last = p;
                touch_idle = 0;
            }
            None if touch_active => {
                touch_idle += 1;
                if touch_idle >= TOUCH_RELEASE_TICKS {
                    touch_active = false;
                    touch_idle = 0;
                    input = true;
                    let dx = touch_last.0 - touch_start.0;
                    let dy = touch_last.1 - touch_start.1;
                    let swiped = dx * dx + dy * dy >= SWIPE_MIN_PX * SWIPE_MIN_PX;
                    let held_long =
                        touch_started_at.elapsed() >= Duration::from_millis(LONG_PRESS_MS);
                    if swiped {
                        if screen == Screen::Trail {
                            let auto = trail.newest().or(home);
                            if let Some(c) = view_center.or(auto) {
                                view_center = Some(draw::pan_center(
                                    c,
                                    dx,
                                    dy,
                                    draw::SCALE_M_PER_PX[scale_idx],
                                ));
                                println!("nostos-fw: pan dx={} dy={}", dx, dy);
                                redraw = true;
                            }
                        }
                    } else if held_long {
                        if screen == Screen::Settings {
                            // 設定画面の長押し＝電源オフ（バッテリ駆動時。USB 給電中は再起動相当）。
                            println!("nostos-fw: POWER OFF (pm1 shutdown)");
                            Timer::after(Duration::from_millis(50)).await;
                            ioe::shutdown(&mut i2c);
                        } else if view_center.is_some() {
                            view_center = None;
                            println!("nostos-fw: recenter");
                            redraw = true;
                        }
                    } else if touch_last.1 >= draw::TAB_Y0 {
                        // 下部タブのタップで画面切替（e-ink 更新中の多重反応防止に 1.5s）。
                        let tab = match touch_last.0 / 160 {
                            0 => Screen::Trail,
                            1 => Screen::Homing,
                            _ => Screen::Settings,
                        };
                        if tab != screen
                            && last_toggle_at.elapsed() >= Duration::from_millis(1500)
                        {
                            screen = tab;
                            last_toggle_at = Instant::now();
                            println!(
                                "nostos-fw: screen -> {}",
                                match screen {
                                    Screen::Trail => "trail",
                                    Screen::Homing => "homing",
                                    Screen::Settings => "settings",
                                }
                            );
                            redraw = true;
                        }
                    } else if screen == Screen::Settings
                        && touch_last.1 >= draw::SETTINGS_AUTOOFF_Y.0
                        && touch_last.1 < draw::SETTINGS_AUTOOFF_Y.1
                    {
                        auto_off = !auto_off;
                        println!("nostos-fw: auto_off -> {}", auto_off as u8);
                        redraw = true;
                    } else if screen == Screen::Settings
                        && touch_last.1 >= draw::SETTINGS_RESET_Y.0
                        && touch_last.1 < draw::SETTINGS_RESET_Y.1
                    {
                        // ウォームリセット（ソフトリセット）。電源レール保持のまま FW を
                        // 再実行＝パネル固着なしで再起動し、microSD を再初期化する。
                        // 電源ボタン（→固着）や PC 無しで SD 挿入後の再初期化ができる。
                        println!("nostos-fw: WARM RESET (software_reset / SD re-init)");
                        Timer::after(Duration::from_millis(80)).await;
                        esp_hal::system::software_reset();
                    } else if screen == Screen::Settings
                        && touch_last.1 >= draw::SETTINGS_SD_Y.0
                        && touch_last.1 < draw::SETTINGS_SD_Y.1
                        && sdlog.is_some()
                    {
                        // SD 取り外し: 最後に `sd_eject` 行を記録してからロガーを破棄する。
                        // 以後は受信/ステータスとも SD に書かないので、カードを抜いてよい。
                        // 再使用は REBOOT 行（起動時にのみ SD を初期化するため）。
                        log_status(
                            &mut sdlog,
                            "sd_eject",
                            &mut i2c,
                            &mut radio,
                            radio_ok,
                            &StatusCtx {
                                last,
                                last_rx_at,
                                vbat_mv: vbat_cache,
                                vin_mv: vin_cache,
                                rx_count,
                                frontlight: if light_dimmed { 0 } else { brightness_idx as u8 },
                                reset_reason,
                            },
                        )
                        .await;
                        sdlog = None;
                        sd_ejected = true;
                        println!("nostos-fw: SD EJECT (logger stopped, safe to remove)");
                        redraw = true;
                    }
                }
            }
            None => {}
        }

        // --- フロントライトの自動消灯 / 操作での復帰 ---
        if input {
            last_input_at = Instant::now();
            if light_dimmed {
                light_dimmed = false;
                if brightness_idx > 0 {
                    ioe::set_frontlight(&mut i2c, brightness_duty(brightness_idx));
                }
            }
        } else if auto_off
            && !light_dimmed
            && brightness_idx > 0
            && last_input_at.elapsed() >= Duration::from_secs(AUTO_OFF_SECS)
        {
            light_dimmed = true;
            ioe::set_frontlight(&mut i2c, 0);
        }

        // --- RGB LED ステータス（優先度: 赤=低電池 > 橙=途絶 > 緑=受信 > 青=給電）---
        led_ctl.update(
            &mut i2c,
            &led::LedInputs {
                age_secs: last_rx_at.map(|t| t.elapsed().as_secs()),
                last_rx_at,
                vbat_mv: vbat_cache,
                vin_mv: vin_cache,
                stale_secs: STALE_REDRAW_SECS,
            },
        );

        // --- 受信が途絶えても AGE 表示を進める（再描画は控えめに）---
        if !redraw
            && last_rx_at.is_some()
            && last_render_at.elapsed() >= Duration::from_secs(STALE_REDRAW_SECS)
        {
            redraw = true;
        }

        if redraw {
            render_and_paint(
                bw,
                red,
                &trail,
                last,
                last_rx_at,
                home,
                scale_idx,
                screen,
                view_center,
                radio_ok,
                (brightness_idx, auto_off, vbat_cache, vin_cache),
                sd_state(&sdlog, sd_ejected),
                &mut panel,
                &mut i2c,
                &busy,
            )
            .await;
            last_render_at = Instant::now();
        }
    }
}

/// 設定画面「SD CARD」行に表示するロガー状態。
fn sd_state(sdlog: &Option<sdlog::SdLogger>, ejected: bool) -> draw::SdState {
    match (sdlog.is_some(), ejected) {
        (true, _) => draw::SdState::Logging,
        (false, true) => draw::SdState::Ejected,
        (false, false) => draw::SdState::NoCard,
    }
}

/// ステータスログ 1 行に載せるメインループ側の状態（`log_status` の引数まとめ）。
struct StatusCtx {
    last: Option<draw::LastRx>,
    last_rx_at: Option<Instant>,
    vbat_mv: Option<u16>,
    vin_mv: Option<u16>,
    rx_count: u32,
    frontlight: u8,
    reset_reason: u8,
}

/// ステータス 1 行を組み立て、シリアルに出力し、SD（あれば）へ `STATUS.CSV` 追記する。
/// PM1 の電源レジスタと SX1262 の瞬時状態はここで読む（呼び出し頻度は 10 分に 1 回程度）。
async fn log_status(
    sdlog: &mut Option<sdlog::SdLogger>,
    event: &str,
    i2c: &mut ioe::SysI2c,
    radio: &mut lora::Radio,
    radio_ok: bool,
    ctx: &StatusCtx,
) {
    let (pwr_src, pwr_cfg) = ioe::read_pm1_power_regs(i2c);
    let radio_st = if radio_ok {
        let (_irq, rssi, raw) = radio.debug_status();
        Some((rssi, raw))
    } else {
        None
    };
    // 壁時計は無い: 最終受信フレームの GPS 時刻 + 経過秒で推定（未受信は 0）。
    let est_unix = match (ctx.last.filter(|rx| rx.time_unix != 0), ctx.last_rx_at) {
        (Some(rx), Some(at)) => u64::from(rx.time_unix) + at.elapsed().as_secs(),
        _ => 0,
    };
    let sample = statuslog::Sample {
        uptime_s: Instant::now().as_secs(),
        est_unix,
        vbat_mv: ctx.vbat_mv,
        vin_mv: ctx.vin_mv,
        pwr_src,
        pwr_cfg,
        radio: radio_st,
        last_rx_age_s: ctx.last_rx_at.map(|t| t.elapsed().as_secs()),
        rx_count: ctx.rx_count,
        frontlight: ctx.frontlight,
        reset_reason: ctx.reset_reason,
        event,
    };
    let mut lb = LineBuf::new();
    if !sample.write_csv(&mut lb) {
        println!("nostos-fw: status line overflow");
        return;
    }
    // シリアルにも同じ行を出す（末尾改行は行に含まれている）。
    if let Ok(txt) = core::str::from_utf8(lb.as_bytes()) {
        esp_println::print!("nostos-status: {}", txt);
    }
    if let Some(logger) = sdlog.as_mut() {
        if !logger.append_status(lb.as_bytes()).await {
            println!("nostos-fw: status sdlog append failed");
        }
    }
}

/// SD ログ 1 行を組み立てる固定長バッファ（`core::fmt::Write` 実装・no-std 用）。
/// ステータス行（最長 ~110 バイト）も収まるサイズ。
struct LineBuf {
    buf: [u8; 160],
    len: usize,
}

impl LineBuf {
    fn new() -> Self {
        Self {
            buf: [0; 160],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl core::fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let b = s.as_bytes();
        let end = self.len + b.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(b);
        self.len = end;
        Ok(())
    }
}

/// ブザー（GPIO42）を 2kHz で `ms` ミリ秒鳴らす（ブロッキング・起動診断用）。
fn beep_blocking(bz: &mut Output<'static>, d: &esp_hal::delay::Delay, ms: u32) {
    for _ in 0..(ms * 2) {
        bz.set_high();
        d.delay_micros(250);
        bz.set_low();
        d.delay_micros(250);
    }
}

/// 現在状態を描画してパネルへ転送する。
#[allow(clippy::too_many_arguments)]
async fn render_and_paint(
    bw: &mut [u8; display::PLANE_BYTES],
    red: &mut [u8; display::PLANE_BYTES],
    trail: &Trail<TRAIL_CAP>,
    last: Option<draw::LastRx>,
    last_rx_at: Option<Instant>,
    home: Option<GeoPoint>,
    scale_idx: usize,
    screen: Screen,
    view_center: Option<GeoPoint>,
    radio_ok: bool,
    // (輝度段階, 自動消灯, VBAT[mV], VIN[mV])
    power_ui: (usize, bool, Option<u16>, Option<u16>),
    sd: draw::SdState,
    panel: &mut Option<panel::Panel>,
    i2c: &mut ioe::SysI2c,
    busy: &Input<'static>,
) {
    // 出発点: HOME フレーム受信済みならその座標、未受信なら最初の受信点を暫定採用。
    let anchor = home.or_else(|| trail.oldest());
    let st = draw::Status {
        last,
        age_secs: last_rx_at.map(|t| t.elapsed().as_secs()),
        m_per_px: draw::SCALE_M_PER_PX[scale_idx],
        home,
        homing: anchor.and_then(|h| trail.homing(h)),
        home_provisional: home.is_none(),
        radio_ok,
        vbat_mv: power_ui.2,
        view_center,
        brightness_idx: power_ui.0,
        auto_off: power_ui.1,
        vin_mv: power_ui.3,
        sd,
    };
    match screen {
        Screen::Trail => {
            draw::render_trail(bw, red, trail, &st);
            if let Some(p) = panel.as_mut() {
                p.paint_mono_fast(i2c, bw, red, busy).await;
            }
        }
        Screen::Homing => {
            draw::render_homing(bw, red, trail, &st);
            if let Some(p) = panel.as_mut() {
                p.paint_gray(i2c, bw, red, busy).await;
            }
        }
        Screen::Settings => {
            draw::render_settings(bw, red, &st);
            if let Some(p) = panel.as_mut() {
                p.paint_mono_fast(i2c, bw, red, busy).await;
            }
        }
    }
}
