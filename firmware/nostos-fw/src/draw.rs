//! 軌跡マップ（Trail）画面の描画。North-up 固定・Portrait0（USB 下・480×800）。
//!
//! `docs/UI.md` 第1画面＋モックアップ（`docs/mockups/ui-preview.html` Screen 1）準拠：
//! - ヘッダ: NOSTOS / 時刻（JST・受信 time_unix＋経過から算出）・電池% / 最終受信からの経過
//! - North-up 実線グリッド・N 方位（左上・塗り矢頭）・スケールバー
//! - 経由点＝**白抜き丸＋右に連番**、出発点＝**家アイコン**、現在地＝黒丸に白抜き穴
//! - 破線コネクタ、下段ステータス（C6L 座標 / seq / 経過 / RSSI / HOME 距離・方位）
//!
//! 差分（意図的）: 衛星数は表示しない（フレーム非搭載・UI.md データ面）。日本語ラベルと
//! 下部タブは日本語フォント埋め込み／タッチ実装と合わせて次段で対応。
//!
//! ページ座標（Portrait0）で描き `display::page_to_framebuffer` で物理プレーンへ落とす
//! （papermono-rs `draw.rs` の GrayInk パターン踏襲・MIT）。モノクロ運用のため
//! 黒＝両プレーンのビットを立てる／白＝ビットを消す。

use core::fmt::Write as _;

use embedded_graphics::mono_font::iso_8859_1::{FONT_10X20, FONT_9X15};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Alignment, Text};
use m5stack_papermono_lite::display::{self, PageRotation};
use nostos_nav::{GeoPoint, Homing, Trail};

/// 固定ページ回転（USB 下・縦持ち）。
const ROT: PageRotation = PageRotation::Portrait0;

/// ページ幅（Portrait0 = 480）。
const PAGE_W: i32 = display::PAGE_PORTRAIT_W as i32;
/// ページ高（Portrait0 = 800）。
const PAGE_H: i32 = display::PAGE_PORTRAIT_H as i32;

// マップ領域（ヘッダ下〜フッタ上。モックアップの区切り 64 / 688 に合わせる）。
const MAP_X0: i32 = 0;
const MAP_Y0: i32 = 66;
const MAP_X1: i32 = PAGE_W - 1;
const MAP_Y1: i32 = 688;

/// グリッド間隔 [px]（スケールバーと連動）。
const GRID_PX: i32 = 80;

/// ズーム段階（1 画素あたりのメートル数）。
pub const SCALE_M_PER_PX: [u32; 7] = [1, 2, 5, 10, 20, 50, 100];

/// 描画に渡す受信ステータス（最新フレームのスナップショット）。
pub struct Status {
    /// 最新の受信フレーム。未受信なら None。
    pub last: Option<LastRx>,
    /// 最終受信からの経過秒。未受信なら None。
    pub age_secs: Option<u64>,
    /// 現在のズーム（m/px）。
    pub m_per_px: u32,
    /// C6L が共有した出発点（HOME フレーム）。未受信なら None。
    pub home: Option<GeoPoint>,
    /// 現在地→HOME の距離・方位（home 未受信なら None）。
    pub homing: Option<Homing>,
    /// SX1262 プローブ成否（false なら受信不能を明示）。
    pub radio_ok: bool,
    /// バッテリ電圧 [mV]（M5PM1 ADC）。読めなければ None。
    pub vbat_mv: Option<u16>,
}

/// 最新受信フレームの表示用スナップショット。
#[derive(Clone, Copy)]
pub struct LastRx {
    /// フレーム連番。
    pub seq: u8,
    /// GPS fix 有効。
    pub fix: bool,
    /// 緯度（度 ×1e7）。
    pub lat_e7: i32,
    /// 経度（度 ×1e7）。
    pub lon_e7: i32,
    /// 測位時刻（unix 秒・UTC）。0 = 不明。
    pub time_unix: u32,
    /// パケット RSSI [dBm]。
    pub rssi: i16,
    /// パケット SNR [dB]。
    pub snr: i8,
}

/// 固定長フォーマットバッファ（no-alloc で `write!` を受ける）。
struct FmtBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> FmtBuf<N> {
    fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> core::fmt::Write for FmtBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let room = N - self.len;
        let n = bytes.len().min(room);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        Ok(())
    }
}

/// 両プレーンへ黒画素を打つ embedded-graphics ターゲット（ページ座標）。
struct Ink<'a> {
    bw: &'a mut [u8],
    red: &'a mut [u8],
}

impl OriginDimensions for Ink<'_> {
    fn size(&self) -> Size {
        Size::new(PAGE_W as u32, PAGE_H as u32)
    }
}

impl DrawTarget for Ink<'_> {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if color == BinaryColor::On {
                set_black(self.bw, self.red, point.x, point.y);
            }
        }
        Ok(())
    }
}

/// ページ座標 `(px, py)` の物理プレーンビットを操作する（範囲外は無視）。
fn set_px(bw: &mut [u8], red: &mut [u8], px: i32, py: i32, black: bool) {
    if px < 0 || py < 0 || px >= PAGE_W || py >= PAGE_H {
        return;
    }
    let Some((x, y)) = display::page_to_framebuffer(px as u16, py as u16, ROT) else {
        return;
    };
    let i = usize::from(y) * display::BYTES_PER_ROW + usize::from(x) / 8;
    let mask = 0x80u8 >> (x % 8);
    if black {
        if let Some(b) = bw.get_mut(i) {
            *b |= mask;
        }
        if let Some(b) = red.get_mut(i) {
            *b |= mask;
        }
    } else {
        if let Some(b) = bw.get_mut(i) {
            *b &= !mask;
        }
        if let Some(b) = red.get_mut(i) {
            *b &= !mask;
        }
    }
}

fn set_black(bw: &mut [u8], red: &mut [u8], px: i32, py: i32) {
    set_px(bw, red, px, py, true);
}

fn in_map(px: i32, py: i32) -> bool {
    px > MAP_X0 && px < MAP_X1 && py > MAP_Y0 && py < MAP_Y1
}

/// マップ領域内に限定した画素操作。
fn set_px_map(bw: &mut [u8], red: &mut [u8], px: i32, py: i32, black: bool) {
    if in_map(px, py) {
        set_px(bw, red, px, py, black);
    }
}

/// 太さ 2px の黒点（マップ内クリップ）。
fn dot2_map(bw: &mut [u8], red: &mut [u8], px: i32, py: i32) {
    set_px_map(bw, red, px, py, true);
    set_px_map(bw, red, px + 1, py, true);
    set_px_map(bw, red, px, py + 1, true);
    set_px_map(bw, red, px + 1, py + 1, true);
}

/// 塗り潰し円（マップ内クリップ）。
fn disk_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32, r: i32, black: bool) {
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                set_px_map(bw, red, cx + dx, cy + dy, black);
            }
        }
    }
}

/// 円環（マップ内クリップ・線幅 2px）。
fn ring_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32, r: i32) {
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 <= r * r && d2 >= (r - 2) * (r - 2) {
                set_px_map(bw, red, cx + dx, cy + dy, true);
            }
        }
    }
}

/// 経由点マーカー：白抜き丸（白フィル＋黒ストローク 2px）。
fn waypoint_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32) {
    disk_map(bw, red, cx, cy, 6, false);
    ring_map(bw, red, cx, cy, 7);
}

/// 現在地マーカー：黒丸（r10）に白抜き穴（r4）。
fn current_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32) {
    disk_map(bw, red, cx, cy, 10, true);
    disk_map(bw, red, cx, cy, 4, false);
}

/// 出発点マーカー：家アイコン（モックアップの ⌂。白フィル＋黒アウトライン）。
fn house_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32) {
    // 白フィル: 屋根（y=-9..3 で幅を線形補間）＋胴体（y=3..14 幅 11）。
    for dy in -9..=14i32 {
        let hw = if dy < 3 { ((dy + 9) * 11) / 12 } else { 11 };
        for dx in -hw..=hw {
            set_px_map(bw, red, cx + dx, cy + dy, false);
        }
    }
    // アウトライン（2px 相当で二重描き）。
    for o in 0..2i32 {
        line_map(bw, red, cx - 11, cy + 3 + o, cx, cy - 9 + o); // 屋根左
        line_map(bw, red, cx + 11, cy + 3 + o, cx, cy - 9 + o); // 屋根右
        line_map(bw, red, cx - 11 + o, cy + 3, cx - 11 + o, cy + 14); // 左壁
        line_map(bw, red, cx + 11 - o, cy + 3, cx + 11 - o, cy + 14); // 右壁
        line_map(bw, red, cx - 11, cy + 14 - o, cx + 11, cy + 14 - o); // 床
    }
}

/// 破線（Bresenham・6 on / 5 off・太さ 2px・マップ内クリップ）。
fn dashed_line_map(bw: &mut [u8], red: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    let mut phase = 0u32;
    loop {
        if phase % 11 < 6 {
            dot2_map(bw, red, x, y);
        }
        phase += 1;
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// 実線（マップ内クリップ・太さ 1px）。
fn line_map(bw: &mut [u8], red: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32) {
    line_impl(bw, red, x0, y0, x1, y1, true);
}

/// 実線（ページ全域・太さ 1px）。
fn line(bw: &mut [u8], red: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32) {
    line_impl(bw, red, x0, y0, x1, y1, false);
}

fn line_impl(bw: &mut [u8], red: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32, clip: bool) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        if clip {
            set_px_map(bw, red, x, y, true);
        } else {
            set_black(bw, red, x, y);
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// 度 ×1e7 → "N35.02250" 形式（半球プレフィクス＋5 桁小数）。
fn write_coord<const N: usize>(out: &mut FmtBuf<N>, e7: i32, pos: char, neg: char) {
    let hemi = if e7 < 0 { neg } else { pos };
    let a = e7.unsigned_abs();
    let _ = write!(out, "{}{}.{:05}", hemi, a / 10_000_000, (a % 10_000_000) / 100);
}

/// 経過秒 → "m:ss"（60 分以上は "h:mm"）。
fn write_age<const N: usize>(out: &mut FmtBuf<N>, age: u64) {
    if age < 3600 {
        let _ = write!(out, "{}:{:02}", age / 60, age % 60);
    } else {
        let _ = write!(out, "{}h{:02}", age / 3600, (age % 3600) / 60);
    }
}

/// 方位角 [度] → 8 方位名。
fn compass8(deg: f64) -> &'static str {
    const NAMES: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    let idx = ((deg + 22.5) / 45.0) as usize % 8;
    NAMES[idx]
}

/// バッテリ電圧 [mV] → 残量%（LiPo 3.30–4.20 V の線形近似）。
fn batt_pct(mv: u16) -> u32 {
    (u32::from(mv).saturating_sub(3300) * 100 / 900).min(100)
}

/// 経度 1 度あたりのメートル（緯度依存）。
fn m_per_deg_lon(lat_deg: f64) -> f64 {
    111_320.0 * libm::cos(lat_deg * core::f64::consts::PI / 180.0)
}

/// 緯度 1 度あたりのメートル。
const M_PER_DEG_LAT: f64 = 111_320.0;

/// GeoPoint → マップ画素（center を基準に North-up）。
fn project(p: GeoPoint, center: GeoPoint, m_per_px: u32) -> (i32, i32) {
    let cx = (MAP_X0 + MAP_X1) / 2;
    let cy = (MAP_Y0 + MAP_Y1) / 2;
    let dx_m = (p.lon - center.lon) * m_per_deg_lon(center.lat);
    let dy_m = (p.lat - center.lat) * M_PER_DEG_LAT;
    let px = cx + (dx_m / f64::from(m_per_px)) as i32;
    let py = cy - (dy_m / f64::from(m_per_px)) as i32;
    (px, py)
}

/// 軌跡マップ画面を両プレーンへ描画する。
pub fn render_trail<const N: usize>(
    bw: &mut [u8],
    red: &mut [u8],
    trail: &Trail<N>,
    st: &Status,
) {
    bw.fill(0x00);
    red.fill(0x00);

    let big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let mid = MonoTextStyle::new(&FONT_9X15, BinaryColor::On);

    // --- ヘッダ: NOSTOS / 時刻・電池 / 最終受信 ---
    {
        let mut ink = Ink { bw, red };
        let _ = Text::new("NOSTOS", Point::new(12, 42), big).draw(&mut ink);

        // 右上 1 行目: 時刻（JST）と電池%。時刻は受信 time_unix＋経過秒から算出。
        let mut l1 = FmtBuf::<24>::new();
        match st.last.filter(|rx| rx.time_unix != 0) {
            Some(rx) => {
                let now = u64::from(rx.time_unix) + st.age_secs.unwrap_or(0) + 9 * 3600;
                let (hh, mm) = ((now % 86_400) / 3600, (now % 3600) / 60);
                let _ = write!(l1, "{:02}:{:02}", hh, mm);
            }
            None => {
                let _ = write!(l1, "--:--");
            }
        }
        match st.vbat_mv {
            Some(mv) if mv > 0 => {
                let _ = write!(l1, " · {}%", batt_pct(mv));
            }
            _ => {
                let _ = write!(l1, " · --%");
            }
        }
        let _ = Text::with_alignment(l1.as_str(), Point::new(PAGE_W - 12, 30), mid, Alignment::Right)
            .draw(&mut ink);

        // 右上 2 行目: 最終受信からの経過。
        let mut l2 = FmtBuf::<24>::new();
        match st.age_secs {
            Some(a) => {
                let _ = write!(l2, "RX ");
                write_age(&mut l2, a);
                let _ = write!(l2, " AGO");
            }
            None => {
                let _ = write!(l2, "RX ---");
            }
        }
        let _ = Text::with_alignment(l2.as_str(), Point::new(PAGE_W - 12, 54), mid, Alignment::Right)
            .draw(&mut ink);
    }
    // 受信インジケータ（黒点）: 直近 90 秒以内の受信で点灯。
    if st.age_secs.is_some_and(|a| a <= 90) {
        for dy in -3..=3i32 {
            for dx in -3..=3i32 {
                if dx * dx + dy * dy <= 9 {
                    set_black(bw, red, PAGE_W - 12 - 96 + dx, 49 + dy);
                }
            }
        }
    }
    line(bw, red, 0, 63, PAGE_W - 1, 63);
    line(bw, red, 0, 64, PAGE_W - 1, 64);

    // --- グリッド（実線 1px・中心基準）---
    let ccx = (MAP_X0 + MAP_X1) / 2;
    let ccy = (MAP_Y0 + MAP_Y1) / 2;
    let mut gx = ccx % GRID_PX;
    while gx < MAP_X1 {
        if gx > MAP_X0 {
            line_map(bw, red, gx, MAP_Y0 + 1, gx, MAP_Y1 - 1);
        }
        gx += GRID_PX;
    }
    let mut gy = ccy % GRID_PX;
    while gy < MAP_Y1 {
        if gy > MAP_Y0 {
            line_map(bw, red, MAP_X0 + 1, gy, MAP_X1 - 1, gy);
        }
        gy += GRID_PX;
    }

    if !st.radio_ok {
        let mut ink = Ink { bw, red };
        let _ = Text::with_alignment(
            "SX1262 NOT RESPONDING",
            Point::new(PAGE_W / 2, MAP_Y0 + 32),
            big,
            Alignment::Center,
        )
        .draw(&mut ink);
    }

    // --- N 方位インジケータ（左上・塗り矢頭）---
    {
        let nx = MAP_X0 + 44;
        let ny = MAP_Y0 + 38;
        for o in 0..2i32 {
            line_map(bw, red, nx + o, ny + 16, nx + o, ny - 8);
        }
        // 塗り三角（apex (nx, ny-18) → 底辺 (nx±5, ny-8)）。
        for dy in -18..=-8i32 {
            let hw = ((dy + 18) * 5) / 10;
            for dx in -hw..=hw {
                set_px_map(bw, red, nx + dx, ny + dy, true);
            }
        }
        let mut ink = Ink { bw, red };
        let _ = Text::with_alignment("N", Point::new(nx, ny + 36), mid, Alignment::Center)
            .draw(&mut ink);
    }

    // --- スケールバー（左下・グリッド 1 マス分）---
    {
        let sb_y = MAP_Y1 - 20;
        let sb_x = MAP_X0 + 40;
        for o in 0..2i32 {
            line_map(bw, red, sb_x, sb_y + o, sb_x + GRID_PX, sb_y + o);
        }
        line_map(bw, red, sb_x, sb_y - 5, sb_x, sb_y + 6);
        line_map(bw, red, sb_x + GRID_PX, sb_y - 5, sb_x + GRID_PX, sb_y + 6);
        let mut s = FmtBuf::<16>::new();
        let m = st.m_per_px * GRID_PX as u32;
        if m >= 1000 {
            let _ = write!(s, "{}.{} km", m / 1000, (m % 1000) / 100);
        } else {
            let _ = write!(s, "{} m", m);
        }
        let mut ink = Ink { bw, red };
        let _ = Text::with_alignment(
            s.as_str(),
            Point::new(sb_x + GRID_PX / 2, sb_y - 10),
            mid,
            Alignment::Center,
        )
        .draw(&mut ink);
    }

    // --- 軌跡（破線＋白抜き経由点＋家＋現在地）---
    if let Some(center) = trail.newest().or(st.home) {
        // 破線コネクタ。
        let mut prev: Option<(i32, i32)> = None;
        for p in trail.iter() {
            let (px, py) = project(p, center, st.m_per_px);
            if let Some((qx, qy)) = prev {
                dashed_line_map(bw, red, qx, qy, px, py);
            }
            prev = Some((px, py));
        }

        // 出発点の家アイコン: HOME 受信済みならその座標、未受信なら最古点。
        let anchor = st.home.or_else(|| trail.oldest());
        let anchor_px = anchor.map(|a| project(a, center, st.m_per_px));
        if let Some((hx, hy)) = anchor_px {
            house_map(bw, red, hx, hy);
        }

        // 経由点（白抜き丸＋右に連番）。最古点が家アイコンと重なる場合は番号のみ左に。
        let n = trail.len();
        for (i, p) in trail.iter().enumerate() {
            let (px, py) = project(p, center, st.m_per_px);
            let newest = i + 1 == n;
            let on_house = st.home.is_none() && i == 0;
            if newest && !on_house {
                current_map(bw, red, px, py);
            } else if !on_house {
                waypoint_map(bw, red, px, py);
            }
            let mut s = FmtBuf::<8>::new();
            let _ = write!(s, "{}", i + 1);
            let mut ink = Ink { bw, red };
            if on_house {
                let _ = Text::with_alignment(
                    s.as_str(),
                    Point::new(px - 16, py + 5),
                    mid,
                    Alignment::Right,
                )
                .draw(&mut ink);
            } else {
                let _ = Text::new(s.as_str(), Point::new(px + 13, py + 5), mid).draw(&mut ink);
            }
        }
    } else {
        let mut ink = Ink { bw, red };
        let _ = Text::with_alignment(
            "WAITING FOR C6L BEACON...",
            Point::new(PAGE_W / 2, (MAP_Y0 + MAP_Y1) / 2),
            big,
            Alignment::Center,
        )
        .draw(&mut ink);
    }

    // --- フッタ（区切り線＋3 行）---
    line(bw, red, 0, MAP_Y1, PAGE_W - 1, MAP_Y1);
    line(bw, red, 0, MAP_Y1 + 1, PAGE_W - 1, MAP_Y1 + 1);
    {
        let mut l1 = FmtBuf::<48>::new();
        match st.last {
            Some(rx) => {
                let _ = write!(l1, "C6L  ");
                write_coord(&mut l1, rx.lat_e7, 'N', 'S');
                let _ = write!(l1, "  ");
                write_coord(&mut l1, rx.lon_e7, 'E', 'W');
            }
            None => {
                let _ = write!(l1, "C6L  ---------  ----------");
            }
        }

        let mut l2 = FmtBuf::<56>::new();
        match st.last {
            Some(rx) => {
                let _ = write!(l2, "SEQ {} · FIX {} · N {} · LAST ", rx.seq, rx.fix as u8, trail.len());
                match st.age_secs {
                    Some(a) => write_age(&mut l2, a),
                    None => {
                        let _ = write!(l2, "---");
                    }
                }
                let _ = write!(l2, " · {} dBm {} dB", rx.rssi, rx.snr);
            }
            None => {
                let _ = write!(l2, "SEQ --- · N 0 · LAST ---");
            }
        }

        let mut l3 = FmtBuf::<48>::new();
        match (st.home, st.homing) {
            (Some(_), Some(h)) => {
                let d = h.distance_m as u32;
                if d >= 1000 {
                    let _ = write!(l3, "HOME {}.{:02} km", d / 1000, (d % 1000) / 10);
                } else {
                    let _ = write!(l3, "HOME {} m", d);
                }
                let _ = write!(l3, " · {}°{}", h.bearing_deg as u32, compass8(h.bearing_deg));
            }
            _ => {
                let _ = write!(l3, "HOME not received");
            }
        }

        let mut ink = Ink { bw, red };
        let _ = Text::new(l1.as_str(), Point::new(16, MAP_Y1 + 30), big).draw(&mut ink);
        let _ = Text::new(l2.as_str(), Point::new(16, MAP_Y1 + 56), mid).draw(&mut ink);
        let _ = Text::new(l3.as_str(), Point::new(16, MAP_Y1 + 84), big).draw(&mut ink);
    }
}
