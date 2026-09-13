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
//!
//! HOME 仕様（`docs/UI.md` 確定事項）:
//! - `FLAG_HOME` 付きフレームで出発点を設定/更新（**Trail には積まない**）
//! - HOME 座標が変わったら新しい行程として Trail をリセット
//! - HOME 未受信の間は「HOME not received」表示（軌跡プロットは通常どおり）
//!
//! 操作: ボタン A（GPIO2）＝ズームイン / ボタン B（GPIO3）＝ズームアウト /
//! 画面タップ（TOUCH_INT GPIO4）＝軌跡 ⇄ 帰路の画面切替（タブ実装までの暫定）。

#![no_std]
#![no_main]

mod draw;
mod ioe;
mod lora;
mod panel;

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

/// 表示中の画面。タップで循環切替。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    /// 第1画面：軌跡マップ（モノクロ・部分更新）。
    Trail,
    /// 第2画面：帰路ナビ（4 階調・常にフル更新）。
    Homing,
}

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
    let mut prev_a = false;
    let mut prev_b = false;
    let mut prev_tp = false;
    let mut last_toggle_at = Instant::now();

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
        radio_ok,
        &mut panel,
        &mut i2c,
        &busy,
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
            println!("nostos-fw: dbg irq=0x{:04x} rssi={} status=0x{:02x}", irq, rssi, raw);
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
                        // 出発点の設定/更新。座標が変われば新しい行程 → Trail リセット。
                        let p = f.geopoint();
                        let changed =
                            home.is_none_or(|h| nostos_nav::haversine_m(h, p) > 1.0);
                        if changed {
                            trail = Trail::new();
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
                    redraw = true;
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

        // --- ボタン（立ち下がりエッジでズーム）---
        let a = btn_a.is_low();
        let b = btn_b.is_low();
        if a && !prev_a && scale_idx > 0 {
            scale_idx -= 1;
            redraw = true;
        }
        if b && !prev_b && scale_idx + 1 < draw::SCALE_M_PER_PX.len() {
            scale_idx += 1;
            redraw = true;
        }
        prev_a = a;
        prev_b = b;

        // --- タップ（TOUCH_INT 立ち下がり）で画面切替。e-ink 更新中の多重反応を
        // 避けるため 1.5 秒のクールダウンを置く ---
        let t = tp.is_low();
        if t && !prev_tp && last_toggle_at.elapsed() >= Duration::from_millis(1500) {
            screen = match screen {
                Screen::Trail => Screen::Homing,
                Screen::Homing => Screen::Trail,
            };
            last_toggle_at = Instant::now();
            println!(
                "nostos-fw: screen -> {}",
                if screen == Screen::Trail { "trail" } else { "homing" }
            );
            redraw = true;
        }
        prev_tp = t;

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
                radio_ok,
                &mut panel,
                &mut i2c,
                &busy,
            )
            .await;
            last_render_at = Instant::now();
        }
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
    radio_ok: bool,
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
        vbat_mv: ioe::read_vbat_mv(i2c),
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
    }
}
