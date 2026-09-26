//! 軌跡 CSV の再生（`--features replay`・画面撮影用の一時機能）。
//!
//! ビルド時にリポジトリ直下の `log/NOSTOS.CSV`（microSD の受信ログそのまま・コミット禁止）を
//! 埋め込み、起動直後に受信時と同じ処理（HOME 設定・Trail 追加）へ流し込む。
//! 環境変数 `NOSTOS_REPLAY_ROWS=<n>` を付けてビルドすると先頭 n 行（ヘッダ除く）で止める
//! （往路の途中＝折り返し地点の帰路画面などを再現する用途）。
//!
//! 再生ビルドでは無線受信と microSD 書き込みを行わない（実ログを汚さないため）。

use nostos_frame::NostosFrame;

/// 埋め込み CSV（`time_unix,seq,fix,home,lat_e7,lon_e7,rssi,snr`）。
const CSV: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../log/NOSTOS.CSV"));

/// 再生する最大行数（`NOSTOS_REPLAY_ROWS`・未指定なら全行）。
fn row_limit() -> usize {
    option_env!("NOSTOS_REPLAY_ROWS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX)
}

/// CSV 1 行を (フレーム, RSSI, SNR) に変換。ヘッダ・空行・不正行は `None`。
fn parse_row(line: &str) -> Option<(NostosFrame, i16, i8)> {
    let mut it = line.trim().split(',');
    let time_unix: u32 = it.next()?.parse().ok()?;
    let seq: u8 = it.next()?.parse().ok()?;
    let fix = it.next()? == "1";
    let home = it.next()? == "1";
    let lat_e7: i32 = it.next()?.parse().ok()?;
    let lon_e7: i32 = it.next()?.parse().ok()?;
    let rssi: i16 = it.next()?.parse().ok()?;
    let snr: i8 = it.next()?.parse().ok()?;
    let f = if home {
        NostosFrame::new_home(seq, lat_e7, lon_e7, time_unix)
    } else {
        NostosFrame::new(seq, lat_e7, lon_e7, time_unix, fix)
    };
    Some((f, rssi, snr))
}

/// 再生対象のフレーム列（受信順）。
pub fn frames() -> impl Iterator<Item = (NostosFrame, i16, i8)> {
    CSV.lines().filter_map(parse_row).take(row_limit())
}
