#!/usr/bin/env python3
"""NOSTOS.CSV（nostos-fw の受信ログ）を GPX 1.1 に変換する。

使い方:
  python firmware/nostos-fw/tools/csv2gpx.py log/NOSTOS.CSV log/NOSTOS_2026-09-17.gpx --date 2026-09-17

- fix=1 の行だけを trkpt にする（C6L 起動直後の fix=0 行は除外）
- FLAG_HOME 行は wpt "HOME" としても出力
- 受信間隔が --split-gap 秒（既定 900）を超えた箇所で trkseg を分ける（C6L 電源断の空白を直線で結ばない）
- seq / rssi / snr は <cmt> に残す
"""
import argparse, csv, datetime

JST = datetime.timezone(datetime.timedelta(hours=9))


def iso(t):
    return datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("csv")
    ap.add_argument("gpx")
    ap.add_argument("--date", help="JST の日付 (YYYY-MM-DD) で絞り込む。省略時は全行")
    ap.add_argument("--split-gap", type=int, default=900)
    a = ap.parse_args()
    day = datetime.date.fromisoformat(a.date) if a.date else None

    pts = []
    for r in csv.DictReader(open(a.csv, encoding="utf-8")):
        if r["fix"] != "1":
            continue
        t = int(r["time_unix"])
        if day and datetime.datetime.fromtimestamp(t, JST).date() != day:
            continue
        pts.append((t, int(r["lat_e7"]) / 1e7, int(r["lon_e7"]) / 1e7,
                    int(r["seq"]), int(r["rssi"]), int(r["snr"]), r["home"] == "1"))
    if not pts:
        raise SystemExit("no fix rows")
    name = f"Nostos {day or 'trail'}"
    out = ['<?xml version="1.0" encoding="UTF-8"?>',
           '<gpx version="1.1" creator="Nostos csv2gpx" xmlns="http://www.topografix.com/GPX/1/1">',
           f"  <metadata><name>{name}</name><time>{iso(pts[0][0])}</time></metadata>"]
    for p in pts:
        if p[6]:
            out.append(f'  <wpt lat="{p[1]:.7f}" lon="{p[2]:.7f}"><time>{iso(p[0])}</time>'
                       f"<name>HOME</name><desc>FLAG_HOME (seq {p[3]})</desc></wpt>")
    out.append(f"  <trk><name>{name}</name><trkseg>")
    prev = None
    for p in pts:
        if prev is not None and p[0] - prev > a.split_gap:
            out.append("  </trkseg><trkseg>")
        out.append(f'    <trkpt lat="{p[1]:.7f}" lon="{p[2]:.7f}"><time>{iso(p[0])}</time>'
                   f"<cmt>seq={p[3]} rssi={p[4]} snr={p[5]}</cmt></trkpt>")
        prev = p[0]
    out += ["  </trkseg></trk>", "</gpx>"]
    with open(a.gpx, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(out) + "\n")
    print(f"{len(pts)} trkpts -> {a.gpx}")


if __name__ == "__main__":
    main()
