//! 帰路ナビの言語非依存ロジック。
//!
//! PaperMono 版と CardputerZero 版で共通化する算出コア:
//! - [`haversine_m`] — 2 点間の大圏距離（メートル）
//! - [`bearing_deg`] — 始点から終点への初期方位角（真北 0°、時計回り 0..360）
//! - [`Trail`] — 固定容量のブレッドクラム軌跡（no-alloc リングバッファ）
//!
//! 座標は WGS84 の度（`f64`）。Meshtastic の `latitude_i` / `longitude_i`（度 ×1e7 の整数）からは
//! [`GeoPoint::from_meshtastic_i`] で生成する。
//!
//! `no_std` / no-alloc。三角関数は [`libm`] を用い、firmware / ホストで同一挙動。

#![no_std]

use core::f64::consts::PI;

/// IUGG 平均地球半径（メートル）。
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

#[inline]
fn to_rad(deg: f64) -> f64 {
    deg * PI / 180.0
}

#[inline]
fn to_deg(rad: f64) -> f64 {
    rad * 180.0 / PI
}

/// WGS84 緯度経度（度）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    /// 緯度（度）。北が正。
    pub lat: f64,
    /// 経度（度）。東が正。
    pub lon: f64,
}

impl GeoPoint {
    /// 度指定で生成。
    #[must_use]
    pub const fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }

    /// 度 ×1e7 の整数座標（`lat_e7`/`lon_e7`）から生成。Meshtastic Position や nostos-frame と共通の表現。
    #[must_use]
    pub fn from_e7(lat_e7: i32, lon_e7: i32) -> Self {
        Self {
            lat: f64::from(lat_e7) / 1e7,
            lon: f64::from(lon_e7) / 1e7,
        }
    }

    /// Meshtastic Position の整数座標（度 ×1e7）から生成。[`GeoPoint::from_e7`] の別名。
    #[must_use]
    pub fn from_meshtastic_i(latitude_i: i32, longitude_i: i32) -> Self {
        Self::from_e7(latitude_i, longitude_i)
    }
}

/// 2 点間の大圏距離（メートル）を haversine 公式で求める。
#[must_use]
pub fn haversine_m(a: GeoPoint, b: GeoPoint) -> f64 {
    let phi1 = to_rad(a.lat);
    let phi2 = to_rad(b.lat);
    let dphi = to_rad(b.lat - a.lat);
    let dlambda = to_rad(b.lon - a.lon);

    let sin_dphi = libm::sin(dphi / 2.0);
    let sin_dlambda = libm::sin(dlambda / 2.0);
    let h = sin_dphi * sin_dphi + libm::cos(phi1) * libm::cos(phi2) * sin_dlambda * sin_dlambda;
    let c = 2.0 * libm::atan2(libm::sqrt(h), libm::sqrt(1.0 - h));
    EARTH_RADIUS_M * c
}

/// 始点 `from` から終点 `to` への初期方位角（度、真北 0°、時計回り、0..360）。
#[must_use]
pub fn bearing_deg(from: GeoPoint, to: GeoPoint) -> f64 {
    let phi1 = to_rad(from.lat);
    let phi2 = to_rad(to.lat);
    let dlambda = to_rad(to.lon - from.lon);

    let y = libm::sin(dlambda) * libm::cos(phi2);
    let x = libm::cos(phi1) * libm::sin(phi2) - libm::sin(phi1) * libm::cos(phi2) * libm::cos(dlambda);
    let theta = libm::atan2(y, x);
    (to_deg(theta) + 360.0) % 360.0
}

/// 固定容量のブレッドクラム軌跡（no-alloc リングバッファ）。
///
/// 容量 `N` を超えると最古の点を捨てる。`push` した順に [`Trail::iter`] で走査できる。
#[derive(Debug)]
pub struct Trail<const N: usize> {
    buf: [GeoPoint; N],
    /// 次に書き込む位置。
    head: usize,
    /// 現在の保持点数（`0..=N`）。
    len: usize,
}

impl<const N: usize> Default for Trail<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Trail<N> {
    /// 空の軌跡を生成。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [GeoPoint::new(0.0, 0.0); N],
            head: 0,
            len: 0,
        }
    }

    /// 点を追加（満杯なら最古を上書き）。
    pub fn push(&mut self, p: GeoPoint) {
        self.buf[self.head] = p;
        self.head = (self.head + 1) % N;
        if self.len < N {
            self.len += 1;
        }
    }

    /// 保持している点数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// 空か。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 最古の点（＝現在バッファ上で最も古い記録）。出発点の近似として使える。
    #[must_use]
    pub fn oldest(&self) -> Option<GeoPoint> {
        if self.len == 0 {
            return None;
        }
        let idx = (self.head + N - self.len) % N;
        Some(self.buf[idx])
    }

    /// 最新の点。
    #[must_use]
    pub fn newest(&self) -> Option<GeoPoint> {
        if self.len == 0 {
            return None;
        }
        Some(self.buf[(self.head + N - 1) % N])
    }

    /// 追加順（古→新）に走査。
    pub fn iter(&self) -> impl Iterator<Item = GeoPoint> + '_ {
        let head = self.head;
        let len = self.len;
        (0..len).map(move |i| {
            let idx = (head + N - len + i) % N;
            self.buf[idx]
        })
    }

    /// 軌跡上の総移動距離（隣接点間 haversine の総和、メートル）。
    #[must_use]
    pub fn path_length_m(&self) -> f64 {
        let mut total = 0.0;
        let mut prev: Option<GeoPoint> = None;
        for p in self.iter() {
            if let Some(q) = prev {
                total += haversine_m(q, p);
            }
            prev = Some(p);
        }
        total
    }

    /// 最新点から `home` への直線距離（メートル）と方位角（度）。
    ///
    /// 帰路ナビの中核。`home` は出発点（例: C6L=車の位置、または [`Trail::oldest`]）。
    #[must_use]
    pub fn homing(&self, home: GeoPoint) -> Option<Homing> {
        let cur = self.newest()?;
        Some(Homing {
            distance_m: haversine_m(cur, home),
            bearing_deg: bearing_deg(cur, home),
        })
    }
}

/// 帰路指示（現在地→出発点の距離・方位）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homing {
    /// 直線距離（メートル）。
    pub distance_m: f64,
    /// 方位角（度、真北 0°、時計回り）。
    pub bearing_deg: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        libm::fabs(a - b) <= tol
    }

    #[test]
    fn distance_london_paris() {
        // London ~ Paris は約 343 km（一般的な参照値）。
        let london = GeoPoint::new(51.5007, -0.1246);
        let paris = GeoPoint::new(48.8567, 2.3508);
        let d = haversine_m(london, paris);
        assert!(approx(d, 343_500.0, 2_000.0), "distance was {d} m");
    }

    #[test]
    fn distance_zero_for_same_point() {
        let p = GeoPoint::new(43.06, 141.35);
        assert!(approx(haversine_m(p, p), 0.0, 1e-6));
    }

    #[test]
    fn bearing_cardinals_from_origin() {
        let o = GeoPoint::new(0.0, 0.0);
        // 真東（経度 +1）→ 約 90°
        assert!(approx(bearing_deg(o, GeoPoint::new(0.0, 1.0)), 90.0, 0.01));
        // 真北（緯度 +1）→ 0°
        assert!(approx(bearing_deg(o, GeoPoint::new(1.0, 0.0)), 0.0, 0.01));
        // 真西 → 270°
        assert!(approx(bearing_deg(o, GeoPoint::new(0.0, -1.0)), 270.0, 0.01));
    }

    #[test]
    fn meshtastic_int_conversion() {
        // 度 ×1e7。札幌近郊 ~ (43.0621, 141.3544)
        let p = GeoPoint::from_meshtastic_i(430_621_000, 1_413_544_000);
        assert!(approx(p.lat, 43.0621, 1e-6));
        assert!(approx(p.lon, 141.3544, 1e-6));
    }

    #[test]
    fn trail_ring_buffer_overwrites_oldest() {
        let mut t: Trail<3> = Trail::new();
        assert!(t.is_empty());
        t.push(GeoPoint::new(0.0, 0.0));
        t.push(GeoPoint::new(0.0, 1.0));
        t.push(GeoPoint::new(0.0, 2.0));
        t.push(GeoPoint::new(0.0, 3.0)); // 最古(0,0)を追い出す
        assert_eq!(t.len(), 3);
        assert_eq!(t.oldest(), Some(GeoPoint::new(0.0, 1.0)));
        assert_eq!(t.newest(), Some(GeoPoint::new(0.0, 3.0)));
        let pts: [GeoPoint; 3] = {
            let mut it = t.iter();
            [it.next().unwrap(), it.next().unwrap(), it.next().unwrap()]
        };
        assert_eq!(pts[0], GeoPoint::new(0.0, 1.0));
        assert_eq!(pts[2], GeoPoint::new(0.0, 3.0));
    }

    #[test]
    fn trail_path_length_and_homing() {
        let mut t: Trail<8> = Trail::new();
        let home = GeoPoint::new(0.0, 0.0);
        t.push(home);
        t.push(GeoPoint::new(0.0, 0.01));
        t.push(GeoPoint::new(0.0, 0.02));
        // 3 点で東へ 0.02 度移動 → path_length ≒ home→現在 の距離
        let straight = haversine_m(home, GeoPoint::new(0.0, 0.02));
        assert!(approx(t.path_length_m(), straight, 1.0));

        let h = t.homing(home).unwrap();
        assert!(approx(h.distance_m, straight, 1.0));
        // 現在地(東)から home(西)へ帰る → 方位 ~270°
        assert!(approx(h.bearing_deg, 270.0, 0.5));
    }
}
