//! 画面描画。North-up 固定・Portrait0（USB 下・480×800）。
//!
//! `docs/UI.md`＋モックアップ（`docs/mockups/ui-preview.html`）準拠の 2 画面：
//! - [`render_trail`] 軌跡マップ（モノクロ・部分更新前提）
//! - [`render_homing`] 帰路ナビ（4 階調。現在地中心・薄墨の来た道・濃破線の帰路方位）
//!
//! ページ座標（Portrait0）で描き `display::page_to_framebuffer` で物理プレーンへ落とす
//! （papermono-rs `draw.rs` の GrayInk パターン踏襲・MIT）。プレーンのビットは
//! `gray_planes` のエンコード（WHITE=(0,0)／LIGHT=bw／DARK=red／BLACK=(1,1)）。
//! モノクロ描画（`paint_mono_fast`）はどちらかのビットが立った画素を黒として扱うため、
//! 軌跡画面は BLACK/WHITE のみ使う。
//!
//! 日本語ラベル（軌跡・出発地点等）と下部タブは日本語フォント埋め込み／タッチ座標
//! デコードと合わせて次段対応（暫定：画面切替はタップ、画面名はヘッダに英字表示）。

use core::fmt::Write as _;

// 中サイズ英数字は太字（e-ink では 1px ストロークが灰色に見えるため。同寸なのでレイアウト不変）。
use embedded_graphics::mono_font::iso_8859_1::{FONT_10X20, FONT_9X15_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Alignment, Text};
use m5stack_papermono_lite::display::{self, PageRotation};
use nostos_nav::{bearing_deg, GeoPoint, Homing, Trail};

use crate::jpfont;

/// 固定ページ回転（USB 下・縦持ち）。
const ROT: PageRotation = PageRotation::Portrait0;

/// ページ幅（Portrait0 = 480）。
const PAGE_W: i32 = display::PAGE_PORTRAIT_W as i32;
/// ページ高（Portrait0 = 800）。
const PAGE_H: i32 = display::PAGE_PORTRAIT_H as i32;

// マップ領域（ヘッダ下〜フッタ上。モックアップの区切り 64 / 688 / 760 に合わせる）。
const MAP_X0: i32 = 0;
const MAP_Y0: i32 = 66;
const MAP_X1: i32 = PAGE_W - 1;
const MAP_Y1: i32 = 688;

/// 下部タブバーの上端 y（タブ高 40px・3 分割）。タップ判定にも使う。
pub const TAB_Y0: i32 = 760;

/// 設定画面「自動消灯」行のタップ判定 y 範囲（ページ座標）。
pub const SETTINGS_AUTOOFF_Y: (i32, i32) = (236, 296);
/// 設定画面「記録」行（軌跡の記録 一時停止/再開・行タップでトグル）のタップ判定 Y 範囲。
pub const SETTINGS_REC_Y: (i32, i32) = (296, 356);

/// 設定画面「再起動（ウォーム・SD 再初期化）」行のタップ判定 y 範囲（ページ座標）。
pub const SETTINGS_RESET_Y: (i32, i32) = (512, 562);

/// 設定画面「SD 取り外し（ロガー停止）」行のタップ判定 y 範囲（ページ座標）。
pub const SETTINGS_SD_Y: (i32, i32) = (562, 612);

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
    /// 現在地→出発点の距離・方位。出発点は HOME フレーム、未受信なら最初の受信点（暫定）。
    pub homing: Option<Homing>,
    /// [`Self::homing`] の基準が暫定（最初の受信点）か。true なら「HOME*」表記。
    pub home_provisional: bool,
    /// SX1262 プローブ成否（false なら受信不能を明示）。
    pub radio_ok: bool,
    /// バッテリ電圧 [mV]（M5PM1 ADC）。読めなければ None。
    pub vbat_mv: Option<u16>,
    /// スワイプパン中の固定表示中心（軌跡マップのみ）。None = 最新受信点へ自動追従。
    pub view_center: Option<GeoPoint>,
    /// フロントライト輝度段階（0=OFF〜4=最大。設定画面用）。
    pub brightness_idx: usize,
    /// 自動消灯（無操作 30 秒でフロントライト OFF）。
    pub auto_off: bool,
    /// VIN 電圧 [mV]（USB 給電検出・設定画面用）。
    pub vin_mv: Option<u16>,
    /// microSD ロガーの状態（設定画面「SD」行の表示用）。
    pub sd: SdState,
    /// 軌跡の記録を一時停止中か（受信・ログは継続、Trail への追加だけ止める）。
    pub trail_paused: bool,
}

/// microSD ロガーの状態。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SdState {
    /// 起動時にカード無し／初期化失敗（REBOOT で再試行）。
    NoCard,
    /// 記録中（カードを抜いてはいけない）。
    Logging,
    /// 「SD」行タップでロガー停止済み。カードを抜いてよい（再使用は REBOOT）。
    Ejected,
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

/// 指定トーンで画素を打つ embedded-graphics ターゲット（ページ座標）。
struct Ink<'a> {
    bw: &'a mut [u8],
    red: &'a mut [u8],
    tone: u8,
}

impl<'a> Ink<'a> {
    fn black(bw: &'a mut [u8], red: &'a mut [u8]) -> Self {
        Self {
            bw,
            red,
            tone: display::GRAY_BLACK,
        }
    }
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
                set_tone(self.bw, self.red, point.x, point.y, self.tone);
            }
        }
        Ok(())
    }
}

/// ページ座標 `(px, py)` へ 4 階調トーンを書く（範囲外は無視）。
fn set_tone(bw: &mut [u8], red: &mut [u8], px: i32, py: i32, tone: u8) {
    if px < 0 || py < 0 || px >= PAGE_W || py >= PAGE_H {
        return;
    }
    let Some((x, y)) = display::page_to_framebuffer(px as u16, py as u16, ROT) else {
        return;
    };
    let i = usize::from(y) * display::BYTES_PER_ROW + usize::from(x) / 8;
    let mask = 0x80u8 >> (x % 8);
    let (p1, p2) = display::gray_planes(tone);
    if let Some(b) = bw.get_mut(i) {
        if p1 {
            *b |= mask;
        } else {
            *b &= !mask;
        }
    }
    if let Some(b) = red.get_mut(i) {
        if p2 {
            *b |= mask;
        } else {
            *b &= !mask;
        }
    }
}

fn in_map(px: i32, py: i32) -> bool {
    px > MAP_X0 && px < MAP_X1 && py > MAP_Y0 && py < MAP_Y1
}

/// マップ領域内に限定したトーン書き込み。
fn set_tone_map(bw: &mut [u8], red: &mut [u8], px: i32, py: i32, tone: u8) {
    if in_map(px, py) {
        set_tone(bw, red, px, py, tone);
    }
}

/// 太さ 2px の点（マップ内クリップ）。
fn dot2_map(bw: &mut [u8], red: &mut [u8], px: i32, py: i32, tone: u8) {
    set_tone_map(bw, red, px, py, tone);
    set_tone_map(bw, red, px + 1, py, tone);
    set_tone_map(bw, red, px, py + 1, tone);
    set_tone_map(bw, red, px + 1, py + 1, tone);
}

/// 塗り潰し円（マップ内クリップ）。
fn disk_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32, r: i32, tone: u8) {
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                set_tone_map(bw, red, cx + dx, cy + dy, tone);
            }
        }
    }
}

/// 円環（マップ内クリップ・線幅 2px）。
fn ring_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32, r: i32, tone: u8) {
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 <= r * r && d2 >= (r - 2) * (r - 2) {
                set_tone_map(bw, red, cx + dx, cy + dy, tone);
            }
        }
    }
}

/// 破線円（マップ内クリップ・1px・2 on / 4 off 相当）。
fn dashed_ring_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32, r: i32, tone: u8) {
    let mut deg = 0i32;
    while deg < 360 {
        if deg % 9 < 4 {
            let rad = f64::from(deg) * core::f64::consts::PI / 180.0;
            let px = cx + (f64::from(r) * libm::cos(rad)) as i32;
            let py = cy - (f64::from(r) * libm::sin(rad)) as i32;
            set_tone_map(bw, red, px, py, tone);
        }
        deg += 1;
    }
}

/// 経由点マーカー：白抜き丸（白フィル＋ストローク 2px）。
fn waypoint_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32, tone: u8) {
    disk_map(bw, red, cx, cy, 6, display::GRAY_WHITE);
    ring_map(bw, red, cx, cy, 7, tone);
}

/// 現在地マーカー：塗り丸（r10）に白抜き穴（r4）。
fn current_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32) {
    disk_map(bw, red, cx, cy, 10, display::GRAY_BLACK);
    disk_map(bw, red, cx, cy, 4, display::GRAY_WHITE);
}

/// 出発点マーカー：家アイコン（白フィル＋黒アウトライン）。
fn house_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32) {
    for dy in -9..=14i32 {
        let hw = if dy < 3 { ((dy + 9) * 11) / 12 } else { 11 };
        for dx in -hw..=hw {
            set_tone_map(bw, red, cx + dx, cy + dy, display::GRAY_WHITE);
        }
    }
    for o in 0..2i32 {
        line_map(bw, red, cx - 11, cy + 3 + o, cx, cy - 9 + o, display::GRAY_BLACK);
        line_map(bw, red, cx + 11, cy + 3 + o, cx, cy - 9 + o, display::GRAY_BLACK);
        line_map(bw, red, cx - 11 + o, cy + 3, cx - 11 + o, cy + 14, display::GRAY_BLACK);
        line_map(bw, red, cx + 11 - o, cy + 3, cx + 11 - o, cy + 14, display::GRAY_BLACK);
        line_map(bw, red, cx - 11, cy + 14 - o, cx + 11, cy + 14 - o, display::GRAY_BLACK);
    }
}

/// 出発点マーカー（帰路画面・大）：黒塗り家＋白ドア（モックアップ準拠）。
fn house_big_map(bw: &mut [u8], red: &mut [u8], cx: i32, cy: i32) {
    // 黒フィル: 屋根（y=-10..4）＋胴体（y=4..16 幅 12）。
    for dy in -10..=16i32 {
        let hw = if dy < 4 { ((dy + 10) * 12) / 14 } else { 12 };
        for dx in -hw..=hw {
            set_tone_map(bw, red, cx + dx, cy + dy, display::GRAY_BLACK);
        }
    }
    // 白ドア。
    for dy in 6..=16i32 {
        for dx in -6..=6i32 {
            set_tone_map(bw, red, cx + dx, cy + dy, display::GRAY_WHITE);
        }
    }
}

/// 破線（Bresenham・on/off 指定・太さ 2px・マップ内クリップ）。
fn dashed_line_map(
    bw: &mut [u8],
    red: &mut [u8],
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    on: u32,
    off: u32,
    tone: u8,
) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    let mut phase = 0u32;
    loop {
        if phase % (on + off) < on {
            dot2_map(bw, red, x, y, tone);
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

/// 実線（マップ内クリップ・太さ指定 1〜3px）。
fn line_thick_map(
    bw: &mut [u8],
    red: &mut [u8],
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    thick: i32,
    tone: u8,
) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        match thick {
            1 => set_tone_map(bw, red, x, y, tone),
            2 => dot2_map(bw, red, x, y, tone),
            _ => {
                for oy in -1..=1i32 {
                    for ox in -1..=1i32 {
                        set_tone_map(bw, red, x + ox, y + oy, tone);
                    }
                }
            }
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

fn line_map(bw: &mut [u8], red: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32, tone: u8) {
    line_thick_map(bw, red, x0, y0, x1, y1, 1, tone);
}

/// 実線（ページ全域・太さ 1px・黒）。
fn line(bw: &mut [u8], red: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        set_tone(bw, red, x, y, display::GRAY_BLACK);
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

/// 距離 [m] → "495 m" / "1.24 km"。
fn write_dist<const N: usize>(out: &mut FmtBuf<N>, m: u32) {
    if m >= 1000 {
        let _ = write!(out, "{}.{:02} km", m / 1000, (m % 1000) / 10);
    } else {
        let _ = write!(out, "{} m", m);
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

/// スワイプ量 [px] だけ表示中心をずらした新しい中心を返す（コンテンツが指に追従する向き）。
///
/// 指を右へ動かす（+dx）と地図が右へ流れ、表示中心は西（-lon）へ動く。
/// 指を下へ動かす（+dy）と表示中心は北（+lat）へ動く（North-up）。
pub fn pan_center(center: GeoPoint, dx_px: i32, dy_px: i32, m_per_px: u32) -> GeoPoint {
    let m = f64::from(m_per_px);
    let north_m = f64::from(dy_px) * m;
    let east_m = -f64::from(dx_px) * m;
    GeoPoint::new(
        center.lat + north_m / M_PER_DEG_LAT,
        center.lon + east_m / m_per_deg_lon(center.lat),
    )
}

/// 直近の進路（course made good）[度]。trail が 2 点未満なら None。
fn course_deg<const N: usize>(trail: &Trail<N>) -> Option<f64> {
    let n = trail.len();
    if n < 2 {
        return None;
    }
    let mut prev: Option<GeoPoint> = None;
    let mut cur: Option<GeoPoint> = None;
    for p in trail.iter() {
        prev = cur;
        cur = Some(p);
    }
    Some(bearing_deg(prev?, cur?))
}

// ---------------------------------------------------------------------------
// 日本語テキスト（16×16 グリフ・tools/gen_jpfont.py 生成）
// ---------------------------------------------------------------------------

/// 日本語文字列を描く（未収録文字はスキップ）。戻り値は描画幅 [px]。
fn draw_jp(bw: &mut [u8], red: &mut [u8], x: i32, y: i32, text: &str, tone: u8) -> i32 {
    let mut cx = x;
    for ch in text.chars() {
        if let Some(g) = jpfont::glyph(ch) {
            for row in 0..jpfont::GLYPH_H {
                let bits = u16::from_be_bytes([g[(row * 2) as usize], g[(row * 2 + 1) as usize]]);
                for col in 0..jpfont::GLYPH_W {
                    if bits & (0x8000 >> col) != 0 {
                        set_tone(bw, red, cx + col, y + row, tone);
                    }
                }
            }
            cx += jpfont::GLYPH_W;
        }
    }
    cx - x
}

/// 下部 3 タブ（軌跡 / 帰路 / 設定）。`active` のタブは黒地に白抜き。
fn draw_tabs(bw: &mut [u8], red: &mut [u8], active: usize) {
    line(bw, red, 0, TAB_Y0, PAGE_W - 1, TAB_Y0);
    line(bw, red, 0, TAB_Y0 + 1, PAGE_W - 1, TAB_Y0 + 1);
    line(bw, red, 160, TAB_Y0, 160, PAGE_H - 1);
    line(bw, red, 320, TAB_Y0, 320, PAGE_H - 1);

    const LABELS: [&str; 3] = ["軌跡", "帰路", "設定"];
    for (i, label) in LABELS.iter().enumerate() {
        let x0 = 160 * i as i32;
        let tone = if i == active {
            // 黒地に白抜き。
            for py in TAB_Y0 + 2..PAGE_H {
                for px in x0..x0 + 160 {
                    set_tone(bw, red, px, py, display::GRAY_BLACK);
                }
            }
            display::GRAY_WHITE
        } else {
            display::GRAY_BLACK
        };
        // 2 文字 ×16px = 32px をセル中央へ。
        draw_jp(bw, red, x0 + (160 - 32) / 2, TAB_Y0 + 12, label, tone);
    }
}

// ---------------------------------------------------------------------------
// 共通パーツ
// ---------------------------------------------------------------------------

/// ヘッダ（NOSTOS＋画面名 / 時刻・電池% / 受信経過＋インジケータ）と区切り線。
fn draw_header(bw: &mut [u8], red: &mut [u8], st: &Status, screen: &str) {
    let big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let mid = MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On);
    {
        let mut ink = Ink::black(bw, red);
        let _ = Text::new("NOSTOS", Point::new(12, 42), big).draw(&mut ink);
        let _ = Text::new(screen, Point::new(88, 42), mid).draw(&mut ink);
        if st.trail_paused {
            // 軌跡の記録 一時停止中（設定画面「記録」行）。画面名の右に明示する。
            let x = 88 + screen.len() as i32 * 9 + 12;
            let _ = Text::new("PAUSED", Point::new(x, 42), mid).draw(&mut ink);
        }

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
    if st.age_secs.is_some_and(|a| a <= 90) {
        for dy in -3..=3i32 {
            for dx in -3..=3i32 {
                if dx * dx + dy * dy <= 9 {
                    set_tone(bw, red, PAGE_W - 12 - 96 + dx, 49 + dy, display::GRAY_BLACK);
                }
            }
        }
    }
    line(bw, red, 0, 63, PAGE_W - 1, 63);
    line(bw, red, 0, 64, PAGE_W - 1, 64);
}

/// グリッド（実線 1px・中心基準）と N 方位インジケータ（左上）。
fn draw_grid_and_north(bw: &mut [u8], red: &mut [u8], grid_tone: u8) {
    let ccx = (MAP_X0 + MAP_X1) / 2;
    let ccy = (MAP_Y0 + MAP_Y1) / 2;
    let mut gx = ccx % GRID_PX;
    while gx < MAP_X1 {
        if gx > MAP_X0 {
            line_map(bw, red, gx, MAP_Y0 + 1, gx, MAP_Y1 - 1, grid_tone);
        }
        gx += GRID_PX;
    }
    let mut gy = ccy % GRID_PX;
    while gy < MAP_Y1 {
        if gy > MAP_Y0 {
            line_map(bw, red, MAP_X0 + 1, gy, MAP_X1 - 1, gy, grid_tone);
        }
        gy += GRID_PX;
    }

    let nx = MAP_X0 + 44;
    let ny = MAP_Y0 + 38;
    for o in 0..2i32 {
        line_map(bw, red, nx + o, ny + 16, nx + o, ny - 8, display::GRAY_BLACK);
    }
    for dy in -18..=-8i32 {
        let hw = ((dy + 18) * 5) / 10;
        for dx in -hw..=hw {
            set_tone_map(bw, red, nx + dx, ny + dy, display::GRAY_BLACK);
        }
    }
    let mid = MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On);
    let mut ink = Ink::black(bw, red);
    let _ = Text::with_alignment("N", Point::new(nx, ny + 36), mid, Alignment::Center)
        .draw(&mut ink);
}

/// スケールバー（左下・グリッド 1 マス分）。
fn draw_scalebar(bw: &mut [u8], red: &mut [u8], m_per_px: u32) {
    let sb_y = MAP_Y1 - 20;
    let sb_x = MAP_X0 + 40;
    for o in 0..2i32 {
        line_map(bw, red, sb_x, sb_y + o, sb_x + GRID_PX, sb_y + o, display::GRAY_BLACK);
    }
    line_map(bw, red, sb_x, sb_y - 5, sb_x, sb_y + 6, display::GRAY_BLACK);
    line_map(bw, red, sb_x + GRID_PX, sb_y - 5, sb_x + GRID_PX, sb_y + 6, display::GRAY_BLACK);
    let mut s = FmtBuf::<16>::new();
    write_dist(&mut s, m_per_px * GRID_PX as u32);
    let mid = MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On);
    let mut ink = Ink::black(bw, red);
    let _ = Text::with_alignment(
        s.as_str(),
        Point::new(sb_x + GRID_PX / 2, sb_y - 10),
        mid,
        Alignment::Center,
    )
    .draw(&mut ink);
}

/// フッタ区切り線（2px）。
fn draw_footer_rule(bw: &mut [u8], red: &mut [u8]) {
    line(bw, red, 0, MAP_Y1, PAGE_W - 1, MAP_Y1);
    line(bw, red, 0, MAP_Y1 + 1, PAGE_W - 1, MAP_Y1 + 1);
}

fn draw_center_notice(bw: &mut [u8], red: &mut [u8], msg: &str) {
    let big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let mut ink = Ink::black(bw, red);
    let _ = Text::with_alignment(
        msg,
        Point::new(PAGE_W / 2, (MAP_Y0 + MAP_Y1) / 2),
        big,
        Alignment::Center,
    )
    .draw(&mut ink);
}

fn draw_radio_warning(bw: &mut [u8], red: &mut [u8], radio_ok: bool) {
    if radio_ok {
        return;
    }
    let big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let mut ink = Ink::black(bw, red);
    let _ = Text::with_alignment(
        "SX1262 NOT RESPONDING",
        Point::new(PAGE_W / 2, MAP_Y0 + 32),
        big,
        Alignment::Center,
    )
    .draw(&mut ink);
}

// ---------------------------------------------------------------------------
// 第1画面：軌跡マップ（モノクロ）
// ---------------------------------------------------------------------------

/// 軌跡マップ画面を両プレーンへ描画する（BLACK/WHITE のみ・`paint_mono_fast` 用）。
pub fn render_trail<const N: usize>(
    bw: &mut [u8],
    red: &mut [u8],
    trail: &Trail<N>,
    st: &Status,
) {
    bw.fill(0x00);
    red.fill(0x00);

    let big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let mid = MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On);

    draw_header(bw, red, st, "TRAIL");
    draw_grid_and_north(bw, red, display::GRAY_BLACK);
    draw_radio_warning(bw, red, st.radio_ok);
    draw_scalebar(bw, red, st.m_per_px);

    // パン中は自動追従を止めた固定中心を明示（長押しで復帰）。
    if st.view_center.is_some() {
        let mut ink = Ink::black(bw, red);
        let _ = Text::with_alignment(
            "PAN (hold=recenter)",
            Point::new(PAGE_W / 2, MAP_Y0 + 24),
            mid,
            Alignment::Center,
        )
        .draw(&mut ink);
    }

    if let Some(center) = st.view_center.or_else(|| trail.newest()).or(st.home) {
        let mut prev: Option<(i32, i32)> = None;
        for p in trail.iter() {
            let (px, py) = project(p, center, st.m_per_px);
            if let Some((qx, qy)) = prev {
                dashed_line_map(bw, red, qx, qy, px, py, 6, 5, display::GRAY_BLACK);
            }
            prev = Some((px, py));
        }

        let anchor = st.home.or_else(|| trail.oldest());
        if let Some(a) = anchor {
            let (hx, hy) = project(a, center, st.m_per_px);
            house_map(bw, red, hx, hy);
        }

        let n = trail.len();
        for (i, p) in trail.iter().enumerate() {
            let (px, py) = project(p, center, st.m_per_px);
            let newest = i + 1 == n;
            let on_house = st.home.is_none() && i == 0;
            if newest && !on_house {
                current_map(bw, red, px, py);
            } else if !on_house {
                waypoint_map(bw, red, px, py, display::GRAY_BLACK);
            }
            let mut s = FmtBuf::<8>::new();
            let _ = write!(s, "{}", i + 1);
            let mut ink = Ink::black(bw, red);
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
        draw_center_notice(bw, red, "WAITING FOR C6L BEACON...");
    }

    // フッタ（3 行）。
    draw_footer_rule(bw, red);
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
        match st.homing {
            Some(h) => {
                let _ = write!(l3, "HOME{} ", if st.home_provisional { "*" } else { "" });
                write_dist(&mut l3, h.distance_m as u32);
                let _ = write!(l3, " · {}°{}", h.bearing_deg as u32, compass8(h.bearing_deg));
                if st.home_provisional {
                    let _ = write!(l3, "  (*=first fix)");
                }
            }
            None => {
                let _ = write!(l3, "HOME ---");
            }
        }

        let mut ink = Ink::black(bw, red);
        let _ = Text::new(l1.as_str(), Point::new(16, MAP_Y1 + 24), big).draw(&mut ink);
        let _ = Text::new(l2.as_str(), Point::new(16, MAP_Y1 + 46), mid).draw(&mut ink);
        let _ = Text::new(l3.as_str(), Point::new(16, MAP_Y1 + 66), mid).draw(&mut ink);
    }

    draw_tabs(bw, red, 0);
}

// ---------------------------------------------------------------------------
// 第2画面：帰路ナビ（4 階調）
// ---------------------------------------------------------------------------

/// 帰路ナビ画面を両プレーンへ描画する（4 階調・`paint_gray` 用）。
///
/// - 現在地（最新受信点）を中心に固定した North-up グリッド＋薄墨レンジリング
/// - 来た道（Trail 全体）を**薄墨（DARK）**の実線で重ね描き
/// - 現在地 → HOME の**帰路方位を濃い破線**で指示、HOME は家アイコン＋距離/方位
/// - HOME がマップ外なら方位方向のマップ端へクランプして表示（距離表示で補完）
pub fn render_homing<const N: usize>(
    bw: &mut [u8],
    red: &mut [u8],
    trail: &Trail<N>,
    st: &Status,
) {
    bw.fill(0x00);
    red.fill(0x00);

    let big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let mid = MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On);

    draw_header(bw, red, st, "HOMING");
    // 4 階調が使えるのでグリッドは薄墨で引く（モックアップの淡い格子に対応）。
    draw_grid_and_north(bw, red, display::GRAY_LIGHT);
    draw_radio_warning(bw, red, st.radio_ok);
    draw_scalebar(bw, red, st.m_per_px);

    let ccx = (MAP_X0 + MAP_X1) / 2;
    let ccy = (MAP_Y0 + MAP_Y1) / 2;

    match trail.newest() {
        Some(center) => {
            // レンジリング（薄墨・破線・グリッド 1〜3 マス半径）。
            for k in 1..=3i32 {
                dashed_ring_map(bw, red, ccx, ccy, GRID_PX * k, display::GRAY_LIGHT);
            }

            // 来た道（薄墨の実線 3px＋経由点ドット）。実機の DARK はほぼ黒に見えるため
            // 仕様どおり LIGHT（薄墨）で描く（グリッドと同トーンだが 3px 幅で判別可）。
            let mut prev: Option<(i32, i32)> = None;
            for p in trail.iter() {
                let (px, py) = project(p, center, st.m_per_px);
                if let Some((qx, qy)) = prev {
                    line_thick_map(bw, red, qx, qy, px, py, 3, display::GRAY_LIGHT);
                }
                prev = Some((px, py));
            }
            let n = trail.len();
            for (i, p) in trail.iter().enumerate() {
                if i + 1 == n {
                    continue;
                }
                let (px, py) = project(p, center, st.m_per_px);
                disk_map(bw, red, px, py, 4, display::GRAY_LIGHT);
            }

            // 出発点と帰路方位。HOME フレーム未受信の間は最初の受信点を暫定 HOME* とする。
            let anchor = st.home.or_else(|| trail.oldest());
            if let Some(home) = anchor {
                let (raw_x, raw_y) = project(home, center, st.m_per_px);
                // ラベルが収まるようマップ端へクランプ（マージン付き）。
                let (hx, hy) = clamp_to_map(ccx, ccy, raw_x, raw_y, 90);

                // 帰路方位（濃い破線 8 on / 5 off）。
                dashed_line_map(bw, red, ccx, ccy, hx, hy, 8, 5, display::GRAY_BLACK);

                // HOME マーカー（黒家＋白ドア＋破線リング＋HOME/HOME* ラベル）。
                dashed_ring_map(bw, red, hx, hy, 22, display::GRAY_BLACK);
                house_big_map(bw, red, hx, hy);
                {
                    let mut ink = Ink::black(bw, red);
                    let _ = Text::with_alignment(
                        if st.home_provisional { "HOME*" } else { "HOME" },
                        Point::new(hx, hy - 30),
                        mid,
                        Alignment::Center,
                    )
                    .draw(&mut ink);
                }

                // 距離・方位ラベル（マーカー下）。
                if let Some(h) = st.homing {
                    let mut d = FmtBuf::<16>::new();
                    write_dist(&mut d, h.distance_m as u32);
                    let mut b = FmtBuf::<24>::new();
                    let _ = write!(
                        b,
                        "BRG {}° {}",
                        h.bearing_deg as u32,
                        compass8(h.bearing_deg)
                    );
                    let mut ink = Ink::black(bw, red);
                    let _ = Text::with_alignment(
                        d.as_str(),
                        Point::new(hx, hy + 44),
                        big,
                        Alignment::Center,
                    )
                    .draw(&mut ink);
                    let _ = Text::with_alignment(
                        b.as_str(),
                        Point::new(hx, hy + 64),
                        mid,
                        Alignment::Center,
                    )
                    .draw(&mut ink);
                }
            }

            // 現在地（中心・最前面）。
            current_map(bw, red, ccx, ccy);
        }
        None => {
            draw_center_notice(bw, red, "WAITING FOR C6L BEACON...");
        }
    }

    // フッタ（3 行）。
    draw_footer_rule(bw, red);
    {
        let mut l1 = FmtBuf::<48>::new();
        match st.last {
            Some(rx) => {
                let _ = write!(l1, "POS  ");
                write_coord(&mut l1, rx.lat_e7, 'N', 'S');
                let _ = write!(l1, "  ");
                write_coord(&mut l1, rx.lon_e7, 'E', 'W');
            }
            None => {
                let _ = write!(l1, "POS  ---------  ----------");
            }
        }

        let mut l2 = FmtBuf::<56>::new();
        match course_deg(trail) {
            Some(c) => {
                let _ = write!(l2, "CRS {}°{}", c as u32, compass8(c));
            }
            None => {
                let _ = write!(l2, "CRS ---");
            }
        }
        let _ = write!(l2, " · LAST ");
        match st.age_secs {
            Some(a) => write_age(&mut l2, a),
            None => {
                let _ = write!(l2, "---");
            }
        }
        if let Some(rx) = st.last {
            let _ = write!(l2, " · {} dBm {} dB", rx.rssi, rx.snr);
        }

        let mut l3 = FmtBuf::<48>::new();
        match st.homing {
            Some(h) => {
                let _ = write!(l3, "HOME{} ", if st.home_provisional { "*" } else { "" });
                write_dist(&mut l3, h.distance_m as u32);
                let _ = write!(l3, " · {}°{}", h.bearing_deg as u32, compass8(h.bearing_deg));
                if st.home_provisional {
                    let _ = write!(l3, "  (*=first fix)");
                }
            }
            None => {
                let _ = write!(l3, "HOME ---");
            }
        }

        let mut ink = Ink::black(bw, red);
        let _ = Text::new(l1.as_str(), Point::new(16, MAP_Y1 + 24), big).draw(&mut ink);
        let _ = Text::new(l2.as_str(), Point::new(16, MAP_Y1 + 46), mid).draw(&mut ink);
        let _ = Text::new(l3.as_str(), Point::new(16, MAP_Y1 + 66), mid).draw(&mut ink);
    }

    draw_tabs(bw, red, 1);
}

// ---------------------------------------------------------------------------
// 第3画面：設定（モノクロ）
// ---------------------------------------------------------------------------

/// 設定画面（フロントライト輝度・自動消灯・LED 凡例・電源状態）。
pub fn render_settings(bw: &mut [u8], red: &mut [u8], st: &Status) {
    bw.fill(0x00);
    red.fill(0x00);

    let big = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let mid = MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On);

    draw_header(bw, red, st, "CONFIG");

    // --- 明るさ（フロントライト 5 段階・A ▲ / B ▼）---
    draw_jp(bw, red, 16, 100, "明るさ", display::GRAY_BLACK);
    {
        let mut ink = Ink::black(bw, red);
        let s = match st.brightness_idx {
            0 => "OFF",
            1 => "1",
            2 => "2",
            3 => "3",
            _ => "MAX",
        };
        let _ = Text::with_alignment(s, Point::new(PAGE_W - 20, 116), big, Alignment::Right)
            .draw(&mut ink);
    }
    // 4 セグメントのバー（段階 1〜4=MAX を 1 つずつ。塗り＝現在レベル、OFF は全て白）。
    for i in 0..4i32 {
        let x0 = 16 + i * 112;
        let y0 = 132;
        let (x1, y1) = (x0 + 100, y0 + 28);
        line(bw, red, x0, y0, x1, y0);
        line(bw, red, x0, y1, x1, y1);
        line(bw, red, x0, y0, x0, y1);
        line(bw, red, x1, y0, x1, y1);
        if (i as usize) < st.brightness_idx {
            for py in y0 + 3..y1 - 2 {
                for px in x0 + 3..x1 - 2 {
                    set_tone(bw, red, px, py, display::GRAY_BLACK);
                }
            }
        }
    }
    {
        let mut ink = Ink::black(bw, red);
        let _ = Text::new("A + / B -", Point::new(16, 186), mid).draw(&mut ink);
    }

    // --- 自動消灯（タップでトグル）---
    line(bw, red, 8, SETTINGS_AUTOOFF_Y.0, PAGE_W - 8, SETTINGS_AUTOOFF_Y.0);
    draw_jp(bw, red, 16, 252, "自動消灯", display::GRAY_BLACK);
    {
        let mut ink = Ink::black(bw, red);
        let s = if st.auto_off { "ON (30s)" } else { "OFF" };
        let _ = Text::with_alignment(s, Point::new(PAGE_W - 20, 268), big, Alignment::Right)
            .draw(&mut ink);
        let _ = Text::new("(tap row to toggle)", Point::new(16, 288), mid).draw(&mut ink);
    }
    line(bw, red, 8, SETTINGS_AUTOOFF_Y.1, PAGE_W - 8, SETTINGS_AUTOOFF_Y.1);

    // --- 記録（軌跡への追加を一時停止/再開。タップでトグル）---
    // 停車中・帰宅後に GPS のふらつきで軌跡に点が溜まるのを避ける。受信・CSV ログ・LED は
    // 継続し、Trail への push だけ止める（停止中は現在地マーカー・帰路距離も停止時点で固定）。
    draw_jp(bw, red, 16, 312, "記録", display::GRAY_BLACK);
    {
        let mut ink = Ink::black(bw, red);
        let (s, hint) = if st.trail_paused {
            ("PAUSED", "(tap row to resume trail)")
        } else {
            ("ON", "(tap row to pause trail)")
        };
        let _ = Text::with_alignment(s, Point::new(PAGE_W - 20, 328), big, Alignment::Right)
            .draw(&mut ink);
        let _ = Text::new(hint, Point::new(16, 348), mid).draw(&mut ink);
    }
    line(bw, red, 8, SETTINGS_REC_Y.1, PAGE_W - 8, SETTINGS_REC_Y.1);

    // --- 通知 LED 凡例 ---
    draw_jp(bw, red, 16, 372, "凡例", display::GRAY_BLACK);
    {
        let mut ink = Ink::black(bw, red);
        let _ = Text::new(": LED", Point::new(50, 388), mid).draw(&mut ink);
    }
    const LEGEND: [(&str, &str); 4] = [
        ("受信", "GREEN blink"),
        ("途絶", "ORANGE blink"),
        ("低電池", "RED blink"),
        ("充電中", "BLUE on"),
    ];
    for (i, (jp, en)) in LEGEND.iter().enumerate() {
        let y = 404 + i as i32 * 28;
        draw_jp(bw, red, 32, y, jp, display::GRAY_BLACK);
        let mut ink = Ink::black(bw, red);
        let _ = Text::new(en, Point::new(160, y + 13), mid).draw(&mut ink);
    }

    // --- 再起動（ウォーム・SD 再初期化。行タップで実行）---
    // 電源ボタンは固着を起こすため、SD 挿入後の再初期化などはこのソフトリセットで行う。
    line(bw, red, 8, SETTINGS_RESET_Y.0, PAGE_W - 8, SETTINGS_RESET_Y.0);
    {
        let mut ink = Ink::black(bw, red);
        let _ = Text::new("REBOOT", Point::new(16, SETTINGS_RESET_Y.0 + 24), big).draw(&mut ink);
        let _ = Text::with_alignment(
            "WARM",
            Point::new(PAGE_W - 20, SETTINGS_RESET_Y.0 + 24),
            big,
            Alignment::Right,
        )
        .draw(&mut ink);
        let _ = Text::new(
            "(tap row: warm reset / SD re-init)",
            Point::new(16, SETTINGS_RESET_Y.0 + 43),
            mid,
        )
        .draw(&mut ink);
    }
    line(bw, red, 8, SETTINGS_RESET_Y.1, PAGE_W - 8, SETTINGS_RESET_Y.1);

    // --- SD 取り外し（行タップでロガー停止 → カードを抜いてよい状態にする）---
    // 追記は毎回マウント/アンマウントで完結するが、書き込み中に抜く事故を確実に避けるため、
    // 明示的に停止してから抜く。再使用はカード挿入後に REBOOT 行（ウォームリセット）。
    {
        let mut ink = Ink::black(bw, red);
        let _ = Text::new("SD CARD", Point::new(16, SETTINGS_SD_Y.0 + 24), big).draw(&mut ink);
        let (state, hint) = match st.sd {
            SdState::NoCard => ("NO CARD", "(insert card, then REBOOT)"),
            SdState::Logging => ("LOGGING", "(tap row: stop log -> safe to remove)"),
            SdState::Ejected => ("REMOVE OK", "(log stopped; REBOOT to use again)"),
        };
        let _ = Text::with_alignment(
            state,
            Point::new(PAGE_W - 20, SETTINGS_SD_Y.0 + 24),
            big,
            Alignment::Right,
        )
        .draw(&mut ink);
        let _ = Text::new(hint, Point::new(16, SETTINGS_SD_Y.0 + 43), mid).draw(&mut ink);
    }
    line(bw, red, 8, SETTINGS_SD_Y.1, PAGE_W - 8, SETTINGS_SD_Y.1);

    // --- 電源状態 ---
    {
        let mut l = FmtBuf::<48>::new();
        match st.vbat_mv {
            Some(v) if v > 0 => {
                let _ = write!(l, "BAT {}.{:02} V", v / 1000, (v % 1000) / 10);
            }
            _ => {
                let _ = write!(l, "BAT ---");
            }
        }
        match st.vin_mv {
            Some(v) if v > 0 => {
                let _ = write!(l, " · VIN {}.{:02} V", v / 1000, (v % 1000) / 10);
            }
            _ => {
                let _ = write!(l, " · VIN ---");
            }
        }
        let mut ink = Ink::black(bw, red);
        let _ = Text::new(l.as_str(), Point::new(16, 636), mid).draw(&mut ink);
        let _ = Text::new("hold screen 1s = POWER OFF", Point::new(16, 664), mid).draw(&mut ink);
        let _ = Text::new(
            concat!("nostos-fw v", env!("CARGO_PKG_VERSION")),
            Point::new(16, MAP_Y1 + 24),
            mid,
        )
        .draw(&mut ink);
    }
    line(bw, red, 0, MAP_Y1, PAGE_W - 1, MAP_Y1);
    line(bw, red, 0, MAP_Y1 + 1, PAGE_W - 1, MAP_Y1 + 1);

    draw_tabs(bw, red, 2);
}

/// 中心 `(cx, cy)` から `(x, y)` へ向かうベクトルを、マージン付きマップ矩形内に収める。
fn clamp_to_map(cx: i32, cy: i32, x: i32, y: i32, margin: i32) -> (i32, i32) {
    let bx = f64::from((MAP_X1 - margin - MAP_X0 - margin) / 2);
    let by = f64::from((MAP_Y1 - margin - MAP_Y0 - margin) / 2);
    let dx = f64::from(x - cx);
    let dy = f64::from(y - cy);
    let mut t = 1.0f64;
    if libm::fabs(dx) > bx {
        t = t.min(bx / libm::fabs(dx));
    }
    if libm::fabs(dy) > by {
        t = t.min(by / libm::fabs(dy));
    }
    (cx + (dx * t) as i32, cy + (dy * t) as i32)
}
