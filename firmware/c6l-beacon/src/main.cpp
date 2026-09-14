// Nostos C6L ビーコン — GPS の位置＋時刻を「移動検知で適応的な間隔」＋「ボタン任意発信」で
// 生 LoRa 送信する。**毎送信の前に RSSI ベースのキャリアセンス(LBT)**を行い、混信を防ぐ。
// OLED 3 ページ（STATUS/POSITION/HOME）＋ブザー鳴らし分け＋NeoPixel 通知（docs/UI.md アプリA）。
//
// ⚠️ 電波法・技適コンプライアンス（docs/COMPLIANCE.md 厳守）:
//   送信 RF は認証枠に固定（923.000MHz / BW125 / +6dBm）。認証枠外に変更しない。
//   868MHz 等の国外帯域では送信しない。純正アンテナのまま使用する。
//   920MHz は原則キャリアセンス必須（ARIB STD-T108）。下記 LBT を毎送信で実行する。
//
// ピンマップ出典: meshtastic 変種 variants/esp32c6/m5stack_unitc6l/variant.h ＋ M5Unified
//   SX1262: SCK=20 MISO=22 MOSI=21 CS=23 / DIO1=7 BUSY=19 RESET=NC(実体は PI4IO P7) /
//           DIO2=RFスイッチ DIO3=TCXO 3.0V
//   OLED: SPI 接続 SSD1306 64×48（SX1262 と SPI バス共有）CS=6 / DC=18 / RST=15
//   GPS UART: RX=4 TX=5 / ブザー: 11 / LED: NeoPixel RGB ×1（GPIO2）
//   正面ボタン: **GPIO 直結ではない**。PI4IOE5V6408 I/O エキスパンダ（I2C 0x43・
//   SDA=10/SCL=8）の P0（active-low）。variant.h の BUTTON_PIN 9 はコメントアウト＝
//   BUTTON_EXTENDER が実体（M5Unified board_M5UnitC6L の実装で確定・2026-09-13）。

#include <Arduino.h>
#include <math.h>
#include <Wire.h>
#include <RadioLib.h>
#include <TinyGPSPlus.h>
#include <U8g2lib.h>
#include <Adafruit_NeoPixel.h>
#include "nostos_frame.h"

// ---- コンプライアンス固定パラメータ（crates/nostos-frame::radio と一致・変更禁止領域） ----
static const float   TX_FREQ_MHZ   = 923.0f;   // 認証帯 922〜923.4MHz 内
static const float   TX_BW_KHZ     = 125.0f;   // ≤200kHz（認証）
static const int8_t  TX_POWER_DBM  = 6;        // ≤+7dBm(≈5.0mW認証上限)。+22dBmに上げない
static const uint8_t TX_SF         = 9;
static const uint8_t TX_CR         = 5;        // 4/5
static const uint8_t TX_SYNC_WORD  = 0x3A;     // Nostos 独自ネット（規制対象外・両端一致）
static const float   TCXO_VOLT     = 3.0f;     // DIO3 TCXO

// ---- LBT（キャリアセンス）: ARIB STD-T108 v1.5 Part2(20mW以下・CSあり)で確定。本方式で統一 ----
//   §3.4.2: 閾値 -80dBm（受信電力 >= -80dBm なら送信禁止。出力20mW超で更に低下＝Nostosは≤5mWゆえ据置）
//           時間 >=128μs。§3.4.1(2): 連続送信 <400ms・総量 <=360s/時。
//   ※Part3 LDC(CS不要)は不採用。毎送信でキャリアセンスを行う（docs/COMPLIANCE.md §2.5）。
static const float    CS_THRESHOLD_DBM = -80.0f; // これ以上の RSSI は「混雑」→送信しない（原典確定値）
static const int      LBT_MAX_TRIES    = 3;      // 混雑時の再試行回数
static const uint32_t LBT_BACKOFF_MS   = 3000;   // 全滅時に次サイクルまで待つ

// ---- 適応間隔（移動=短 / 停止=広）＋ボタン任意発信（総司様仕様） ----
static const uint32_t MIN_INTERVAL_MS  = 30UL * 1000;    // 移動時の最短送信間隔
static const uint32_t MAX_INTERVAL_MS  = 600UL * 1000;   // 停止時のハートビート間隔（10分）
static const float    MOVE_THRESHOLD_M = 30.0f;          // 前回送信位置からこれ以上動いたら「移動」

// ---- HOME（出発点）共有: docs/UI.md 確定事項 ----
//   長押しで現在地を HOME 確定 → seq リセット＋FLAG_HOME フレーム即時送信。
//   以降は通常送信 HOME_REBROADCAST_EVERY 回ごとに HOME を再放送（後起動の受信機対策）。
static const uint8_t HOME_REBROADCAST_EVERY = 10;

// ---- ボタン操作（docs/UI.md: 短押し=ページ送り / 長押し2s=HOME確定 / ダブル=任意発信） ----
static const uint32_t BTN_LONG_MS     = 2000;
static const uint32_t BTN_DOUBLE_MS   = 350;   // クリック→クリックの最大間隔
static const uint32_t BTN_DEBOUNCE_MS = 30;

// ---- ダミー GPS（E2E 検証専用・屋内で GPS fix 不能なとき） ----
//   RF（周波数/帯域/出力/sync）は一切変更しない＝技適に無関係。位置データ源のみ差し替える。
//   本番ビルド（env:c6l-beacon）は DUMMY_GPS=0 のまま。検証は env:c6l-beacon-dummy を使う。
#ifndef DUMMY_GPS
#define DUMMY_GPS 0
#endif
#if DUMMY_GPS
static const double   DUMMY_BASE_LAT = 35.0000000;   // 出発点（最古点＝homing 基準）
static const double   DUMMY_BASE_LON = 135.0000000;
// ランダムウォーク: 歩幅は一定・方向は毎送信ランダム（直線トラックだと帰路方位線が
// 軌跡の真上に乗り判読できないため・2026-09-13）。HW RNG（esp_random）使用。
static const double   DUMMY_STEP_M   = 70.0;         // 1 送信あたりの移動量 [m]
// 合成タイムスタンプの基準（各送信 +60s）。実 GPS では衛星時刻が入るため無関係。
// OLED/PaperMono の時計表示を現実に近づけるため、おおよそ現在の unix 秒にしておく。
static const uint32_t DUMMY_EPOCH    = 1789285200UL; // ≈2026-09-13 16:40 JST
#endif

// ---- ピン定義（variant.h より） ----
#define PIN_LORA_SCK  20
#define PIN_LORA_MISO 22
#define PIN_LORA_MOSI 21
#define PIN_LORA_CS   23
#define PIN_LORA_DIO1 7
#define PIN_LORA_BUSY 19
#define PIN_GPS_RX    4   // ESP 受信（GPS-TX 接続）
#define PIN_GPS_TX    5
#define PIN_BUZZER    11  // ブザー
#define PIN_OLED_CS   6   // SSD1306 64x48（SPI・SX1262 とバス共有）
#define PIN_OLED_DC   18
#define PIN_OLED_RST  15
#define PIN_NEOPIXEL  2   // NeoPixel RGB ×1
#define PIN_I2C_SDA   10  // 内部 I2C（PI4IOE5V6408 エキスパンダ）
#define PIN_I2C_SCL   8

// PI4IOE5V6408 I/O エキスパンダ（正面ボタン P0 / LNA P5 / RFスイッチ P6 / LoRaリセット P7）
#define PI4IO_ADDR    0x43
#define PI4IO_REG_IN  0x0F  // 入力ステータス（bit0=ボタン・active-low）

SX1262 radio = new Module(PIN_LORA_CS, PIN_LORA_DIO1, RADIOLIB_NC, PIN_LORA_BUSY);
TinyGPSPlus gps;
// 64×48 SPI SSD1306（WEMOS 0.66" シールドと同パネル系＝ER バリアント）。R2=180°回転。
U8G2_SSD1306_64X48_ER_F_4W_HW_SPI oled(U8G2_R2, PIN_OLED_CS, PIN_OLED_DC, PIN_OLED_RST);
Adafruit_NeoPixel led(1, PIN_NEOPIXEL, NEO_GRB + NEO_KHZ800);

// ---- PI4IOE5V6408 アクセス（M5Unified board_M5UnitC6L と同一設定） ----
static bool pi4io_write8(uint8_t reg, uint8_t val) {
  Wire.beginTransmission(PI4IO_ADDR);
  Wire.write(reg);
  Wire.write(val);
  return Wire.endTransmission() == 0;
}

static uint8_t pi4io_read8(uint8_t reg) {
  Wire.beginTransmission(PI4IO_ADDR);
  Wire.write(reg);
  if (Wire.endTransmission(false) != 0) return 0xFF;
  if (Wire.requestFrom((uint8_t)PI4IO_ADDR, (uint8_t)1) != 1) return 0xFF;
  return Wire.read();
}

// M5Unified Power_Class の reg_data_array_for_lorac6 と同一（実績のある構成）:
//   IO_DIR=P5..P7 出力 / OUT=P7(LoRaリセット解除) / P2-P4 High-Z /
//   P0,P1,P6,P7 プルアップ / ボタン入力デフォルト＋割り込みマスク。
static bool pi4io_init() {
  static const uint8_t seq_[][2] = {
    {0x03, 0b11100000}, {0x05, 0b10000000}, {0x07, 0b00011100},
    {0x0D, 0b11000011}, {0x0B, 0b11000011}, {0x09, 0b00000011},
    {0x11, 0b11111100},
  };
  uint8_t id = pi4io_read8(0x01);
  if (id == 0xFF || id == 0) return false;
  for (auto &rv : seq_) {
    if (!pi4io_write8(rv[0], rv[1])) return false;
  }
  return true;
}

// 正面ボタン（PI4IO P0・active-low）。読めないときは「押されていない」扱い。
static bool front_button_pressed() {
  uint8_t v = pi4io_read8(PI4IO_REG_IN);
  if (v == 0xFF) return false;
  return (v & 0x01) == 0;
}

static uint8_t  seq = 0;
static uint32_t last_tx_ms = 0;
static double   last_tx_lat = 0, last_tx_lon = 0;
static bool     have_last = false;
static uint32_t retry_after_ms = 0;

// HOME（出発点）状態。
static bool     home_set = false;
static double   home_lat = 0, home_lon = 0;
static uint8_t  tx_since_home = 0;   // 前回 HOME 放送からの通常送信数

// UI 状態。
static uint8_t  page = 0;            // 0=STATUS / 1=POSITION / 2=HOME
static bool     last_lbt_free = true;
static bool     had_fix = false;     // fix 獲得遷移（ブザー）検出用
static double   course_deg_v = -1.0; // 進路（course made good）。負=未確定
static uint32_t led_off_ms = 0;      // イベント LED の消灯時刻（0=イベントなし）

// TinyGPS の日時（UTC）から unix 秒へ。未確定時は 0。
static uint32_t gps_unix_time() {
  if (!gps.date.isValid() || !gps.time.isValid() || gps.date.year() < 2020) return 0;
  int y = gps.date.year(); unsigned m = gps.date.month(), d = gps.date.day();
  y -= m <= 2;
  const int era = (y >= 0 ? y : y - 399) / 400;
  const unsigned yoe = (unsigned)(y - era * 400);
  const unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
  const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  const long days = (long)era * 146097 + (long)doe - 719468;
  return (uint32_t)(days * 86400L + gps.time.hour() * 3600L +
                    gps.time.minute() * 60L + gps.time.second());
}

// 2 点間距離 [m]（haversine）。移動検知・HOME 距離用。
static double dist_m(double la1, double lo1, double la2, double lo2) {
  const double R = 6371008.8, d2r = M_PI / 180.0;
  double p1 = la1 * d2r, p2 = la2 * d2r, dp = (la2 - la1) * d2r, dl = (lo2 - lo1) * d2r;
  double a = sin(dp / 2) * sin(dp / 2) + cos(p1) * cos(p2) * sin(dl / 2) * sin(dl / 2);
  return R * 2 * atan2(sqrt(a), sqrt(1 - a));
}

// 始点→終点の初期方位角 [度・真北0・時計回り 0..360]。nostos-nav::bearing_deg と同式。
static double bearing_to(double la1, double lo1, double la2, double lo2) {
  const double d2r = M_PI / 180.0;
  double p1 = la1 * d2r, p2 = la2 * d2r, dl = (lo2 - lo1) * d2r;
  double y = sin(dl) * cos(p2);
  double x = cos(p1) * sin(p2) - sin(p1) * cos(p2) * cos(dl);
  double th = atan2(y, x) / d2r;
  return fmod(th + 360.0, 360.0);
}

// 方位角 → 8 方位名。
static const char *compass8(double deg) {
  static const char *N8[] = {"N", "NE", "E", "SE", "S", "SW", "W", "NW"};
  return N8[(int)((deg + 22.5) / 45.0) % 8];
}

// ---- ブザー鳴らし分け（docs/UI.md 表・短時間ブロッキング許容） ----
static void beep_tx()   { tone(PIN_BUZZER, 2500, 80); }                                  // ピッ（中1）
static void beep_fix()  { tone(PIN_BUZZER, 3200, 60); delay(90); tone(PIN_BUZZER, 3200, 60); } // ピピッ（高2）
static void beep_busy() { tone(PIN_BUZZER, 800, 80);  delay(110); tone(PIN_BUZZER, 800, 80); } // ププッ（低2）
static void beep_home() { tone(PIN_BUZZER, 2000, 500); }                                 // ピーッ（長1）

// ---- NeoPixel（イベント点灯は led_off_ms まで保持。平常時は GPS 探索点滅のみ） ----
static void led_event(uint8_t r, uint8_t g, uint8_t b, uint32_t hold_ms) {
  led.setPixelColor(0, led.Color(r, g, b));
  led.show();
  led_off_ms = millis() + hold_ms;
}

static void led_task(bool fix) {
  uint32_t now = millis();
  if (led_off_ms) {
    if ((int32_t)(now - led_off_ms) < 0) return;  // イベント表示の保持中
    led_off_ms = 0;
  }
  static uint32_t last = 0;
  if (now - last < 250) return;
  last = now;
  // GPS 探索中: 遅い点滅（0.5Hz・青）。fix 確立後: 消灯（省電力）。
  if (!fix && ((now / 1000) % 2 == 0)) {
    led.setPixelColor(0, led.Color(0, 0, 40));
  } else {
    led.setPixelColor(0, 0);
  }
  led.show();
}

// LBT: 送信前エネルギー検出（RSSI）キャリアセンス。空きなら true。
static bool lbt_clear() {
  // ARIB STD-T108 §3.4.2 エネルギー検出: RX 中に GetRssiInst で瞬時チャネル RSSI を読む。
  // ※RadioLib の getRSSI(packet): true=last-packet RSSI / false=瞬時(GetRssiInst)。false を使う。
  radio.startReceive();
  delay(5);                          // 受信機/RSSI 整定（>=128μs 要件も満たす）
  float rssi = radio.getRSSI(false); // GetRssiInst=瞬時チャネル RSSI（エネルギー検出）
  radio.standby();
  bool free_ = rssi < CS_THRESHOLD_DBM;
  last_lbt_free = free_;
  Serial.printf("nostos-beacon: LBT rssi=%.0f dBm -> %s\n", rssi, free_ ? "free" : "busy");
  return free_;                      // < -80dBm なら空き
}

// LBT を挟んで 1 フレーム送信。成功で true。flags は NOSTOS_FLAG_* の OR。
static bool send_frame(uint8_t flags, int32_t lat_e7, int32_t lon_e7, uint32_t t, const char *why) {
  uint8_t frame[NOSTOS_FRAME_LEN];
  nostos_frame_encode_flags(frame, flags, seq, lat_e7, lon_e7, t);
  for (int i = 0; i < LBT_MAX_TRIES; i++) {
    if (lbt_clear()) {
      int st = radio.transmit(frame, NOSTOS_FRAME_LEN);
      bool quiet = strcmp(why, "heartbeat") == 0;  // ハートビートは既定で無音（UI.md）
      if (st == RADIOLIB_ERR_NONE && !quiet) {
        beep_tx();
        led_event(0, 50, 0, 300);   // 緑 1 回フラッシュ＝送信成功
      }
      Serial.printf("nostos-beacon: TX(%s) seq=%u flags=0x%02x lat_e7=%ld lon_e7=%ld t=%lu st=%d\n",
                    why, seq, flags, (long)lat_e7, (long)lon_e7, (unsigned long)t, st);
      seq++;
      return st == RADIOLIB_ERR_NONE;
    }
    delay(20 + i * 40);  // 簡易バックオフ
  }
  Serial.println("nostos-beacon: LBT busy -> skip");
  beep_busy();
  led_event(50, 0, 0, 300);         // 赤点滅相当＝混雑で送信見送り
  return false;
}

// 保存済み HOME 座標を FLAG_HOME 付きで送信。
static bool send_home_frame(uint32_t t, const char *why) {
  if (!home_set) return false;
  bool ok = send_frame(NOSTOS_FLAG_FIX_VALID | NOSTOS_FLAG_HOME,
                       (int32_t)(home_lat * 1e7), (int32_t)(home_lon * 1e7), t, why);
  if (ok) tx_since_home = 0;
  return ok;
}

// ---- OLED 3 ページ描画（64×48・6x12 フォント＝10 桁 ×4 行） ----
static void oled_draw(bool fix, double lat, double lon, uint32_t t, uint32_t now) {
  char l1[16], l2[16], l3[16], l4[16];
  l1[0] = l2[0] = l3[0] = l4[0] = 0;

  switch (page) {
    case 0: {  // STATUS
      snprintf(l1, sizeof(l1), "NOSTOS S%u", seq);
#if DUMMY_GPS
      snprintf(l2, sizeof(l2), "FIX DUMMY");
#else
      if (fix) snprintf(l2, sizeof(l2), "FIX SAT%d", (int)gps.satellites.value());
      else     snprintf(l2, sizeof(l2), "NO FIX");
#endif
      snprintf(l3, sizeof(l3), "LBT %s", last_lbt_free ? "free" : "busy");
      if (t != 0) {
        uint32_t jst = t + 9 * 3600;
        snprintf(l4, sizeof(l4), "%02lu:%02lu:%02lu",
                 (unsigned long)((jst % 86400) / 3600), (unsigned long)((jst % 3600) / 60),
                 (unsigned long)(jst % 60));
      } else if (last_tx_ms != 0) {
        snprintf(l4, sizeof(l4), "TX %lus ago", (unsigned long)((now - last_tx_ms) / 1000));
      } else {
        snprintf(l4, sizeof(l4), "--:--:--");
      }
      break;
    }
    case 1: {  // POSITION
      if (fix) {
        snprintf(l1, sizeof(l1), "%c%9.5f", lat < 0 ? 'S' : 'N', fabs(lat));
        snprintf(l2, sizeof(l2), "%c%9.5f", lon < 0 ? 'W' : 'E', fabs(lon));
#if DUMMY_GPS
        snprintf(l3, sizeof(l3), "ALT ---");
        snprintf(l4, sizeof(l4), "SAT DUMMY");
#else
        snprintf(l3, sizeof(l3), "ALT %dm", (int)gps.altitude.meters());
        snprintf(l4, sizeof(l4), "SAT %d", (int)gps.satellites.value());
#endif
      } else {
        snprintf(l1, sizeof(l1), "POSITION");
        snprintf(l2, sizeof(l2), "NO FIX");
        snprintf(l3, sizeof(l3), "SAT %d", (int)gps.satellites.value());
      }
      break;
    }
    default: {  // HOME
      if (!home_set) {
        snprintf(l1, sizeof(l1), "HOME unset");
        snprintf(l2, sizeof(l2), "hold 2s");
        snprintf(l3, sizeof(l3), "to set");
      } else if (fix) {
        double d = dist_m(lat, lon, home_lat, home_lon);
        double b = bearing_to(lat, lon, home_lat, home_lon);
        if (d >= 1000) snprintf(l1, sizeof(l1), "H %.2fkm", d / 1000.0);
        else           snprintf(l1, sizeof(l1), "H %dm", (int)d);
        snprintf(l2, sizeof(l2), "BRG %d %s", (int)b, compass8(b));
        if (course_deg_v >= 0)
          snprintf(l3, sizeof(l3), "CRS %d %s", (int)course_deg_v, compass8(course_deg_v));
        else
          snprintf(l3, sizeof(l3), "CRS ---");
        snprintf(l4, sizeof(l4), "bcast in %u", (unsigned)(HOME_REBROADCAST_EVERY - tx_since_home));
      } else {
        snprintf(l1, sizeof(l1), "HOME set");
        snprintf(l2, sizeof(l2), "NO FIX");
      }
      break;
    }
  }

  oled.clearBuffer();
  oled.setFont(u8g2_font_6x12_tf);
  oled.drawStr(0, 10, l1);
  oled.drawStr(0, 22, l2);
  oled.drawStr(0, 34, l3);
  oled.drawStr(0, 46, l4);
  oled.sendBuffer();
}

void setup() {
  Serial.begin(115200);
  // USB-Serial-JTAG の CDC はホスト未接続/未読み取りだと printf がブロックし、
  // 長時間運用でビーコンごとフリーズし得る（2026-09-13 実測: 数十分で送信停止）。
  // 送信タイムアウト 0 で「捨てて続行」させる。
  Serial.setTxTimeoutMs(0);
  // GPS UART は 115200bps。M5Stack GPS Unit v1.1(AT6558) の既定は 9600 だが、Meshtastic が
  // 115200 に設定した値がモジュール内に残存しているため 115200 で読む（2026-09-14 実機確認・
  // 9600 だと文字化け＝全バイト不正で fix 不能だった）。将来モジュールを工場既定へ戻すなら 9600。
  Serial1.begin(115200, SERIAL_8N1, PIN_GPS_RX, PIN_GPS_TX);

  // 内部 I2C（PI4IOE5V6408）。正面ボタンはこのエキスパンダの P0。
  Wire.begin(PIN_I2C_SDA, PIN_I2C_SCL, 100000);
  bool ioe_ok = pi4io_init();
  Serial.printf("nostos-beacon: pi4io init %s\n", ioe_ok ? "ok" : "FAILED");

  led.begin();
  led.clear();
  led.show();

  SPI.begin(PIN_LORA_SCK, PIN_LORA_MISO, PIN_LORA_MOSI, PIN_LORA_CS);
  int st = radio.begin(TX_FREQ_MHZ, TX_BW_KHZ, TX_SF, TX_CR, TX_SYNC_WORD,
                       TX_POWER_DBM, 8 /*preamble*/, TCXO_VOLT, false /*LDO*/);
  if (st != RADIOLIB_ERR_NONE) {
    Serial.printf("nostos-beacon: radio.begin failed (%d)\n", st);
    while (true) delay(1000);
  }
  radio.setDio2AsRfSwitch(true);   // variant: DIO2=RFスイッチ

  // OLED は SX1262 と同一 SPI バス（arduino-esp32 の SPI.begin は再入で no-op のため
  // 先に LoRa ピンで begin 済みならそのバスを共有する。CS はそれぞれのドライバが管理）。
  oled.begin();
  oled.setBusClock(4000000);

  Serial.printf("nostos-beacon: ready @ %.3fMHz BW%.0fk SF%d %ddBm sync0x%02X "
                "(move<=%.0fs / stop<=%.0fs / btn: short=page dbl=tx hold=home, LBT on)\n",
                TX_FREQ_MHZ, TX_BW_KHZ, TX_SF, TX_POWER_DBM, TX_SYNC_WORD,
                MIN_INTERVAL_MS / 1000.0, MAX_INTERVAL_MS / 1000.0);
}

void loop() {
  while (Serial1.available()) gps.encode(Serial1.read());

  // ---- ボタン: 短押し=ページ送り / ダブルクリック=任意発信 / 長押し2s=HOME確定 ----
  static bool     btn_stable = false;      // デバウンス済み押下状態
  static bool     btn_raw_prev = false;
  static uint32_t btn_edge_ms = 0;
  static uint32_t btn_down_ms = 0;
  static bool     long_fired = false;
  static uint32_t click_pending_ms = 0;    // 1 回目クリック（ダブル判定待ち）
  bool page_cycle = false, manual = false, home_confirm = false;

  uint32_t now = millis();
  // GPS 診断（USB シリアル・2s 周期）: chars=モジュールが送ってきた総バイト（0 なら UART
  // 無通信＝未接続/未給電/配線/baud）、sats=捕捉衛星数、csumErr=NMEA チェックサム失敗
  // （baud ずれ/ノイズ）。フィールドで GPS がフィックスしない時の切り分け用（USB 未接続時は無害）。
  static uint32_t last_gpsdbg_ms = 0;
  if (now - last_gpsdbg_ms >= 2000) {
    last_gpsdbg_ms = now;
    Serial.printf("nostos-beacon: gps chars=%lu sats=%d withfix=%lu csumErr=%lu valid=%d\n",
                  (unsigned long)gps.charsProcessed(), (int)gps.satellites.value(),
                  (unsigned long)gps.sentencesWithFix(), (unsigned long)gps.failedChecksum(),
                  gps.location.isValid() ? 1 : 0);
  }
  // ボタンは I2C 越しのため 20ms 間隔でポーリング（デバウンス 30ms より十分細かい）。
  static uint32_t last_btn_poll_ms = 0;
  static bool raw_cache = false;
  if (now - last_btn_poll_ms >= 20) {
    last_btn_poll_ms = now;
    raw_cache = front_button_pressed();
  }
  bool raw = raw_cache;
  if (raw != btn_raw_prev) {
    btn_raw_prev = raw;
    btn_edge_ms = now;
  }
  if ((now - btn_edge_ms) >= BTN_DEBOUNCE_MS && raw != btn_stable) {
    btn_stable = raw;
    if (btn_stable) {                      // 押下エッジ
      btn_down_ms = now;
      long_fired = false;
    } else if (!long_fired) {              // 解放エッジ（長押し発火済みなら無視）
      if (click_pending_ms && (now - click_pending_ms) <= BTN_DOUBLE_MS) {
        manual = true;                     // ダブルクリック確定
        click_pending_ms = 0;
      } else {
        click_pending_ms = now;            // 1 回目クリック（ダブル待ち）
      }
    }
  }
  if (btn_stable && !long_fired && (now - btn_down_ms) >= BTN_LONG_MS) {
    long_fired = true;
    home_confirm = true;                   // 長押し確定（押しっぱなしで発火）
    click_pending_ms = 0;
  }
  if (click_pending_ms && (now - click_pending_ms) > BTN_DOUBLE_MS) {
    page_cycle = true;                     // ダブルにならなかった → 短押し確定
    click_pending_ms = 0;
  }

  bool fix = gps.location.isValid();
  double lat = fix ? gps.location.lat() : 0.0;
  double lon = fix ? gps.location.lng() : 0.0;
  int32_t lat_e7 = (int32_t)(lat * 1e7);
  int32_t lon_e7 = (int32_t)(lon * 1e7);
  uint32_t t = gps_unix_time();

#if DUMMY_GPS
  // 屋内 E2E 検証：実 GPS を無視し、ランダムウォークの合成トラックで上書き（fix=1 扱い）。
  // このブロックは loop() ごとに通るため、seq が進んだ回数分だけ歩を進める（冪等）。
  fix = true;
  {
    static double  walk_lat = DUMMY_BASE_LAT;
    static double  walk_lon = DUMMY_BASE_LON;
    static uint8_t walk_done_seq = 0;  // walk_lat/lon が対応する送信 seq
    if (walk_done_seq > seq) {         // seq が u8 で巡回したら基準点へ戻す
      walk_done_seq = 0;
      walk_lat = DUMMY_BASE_LAT;
      walk_lon = DUMMY_BASE_LON;
    }
    while (walk_done_seq < seq) {
      double hdg = (double)esp_random() / 4294967296.0 * TWO_PI;  // 0..2π 一様
      walk_lat += (DUMMY_STEP_M * cos(hdg)) / 111320.0;
      walk_lon += (DUMMY_STEP_M * sin(hdg)) / (111320.0 * cos(walk_lat * DEG_TO_RAD));
      walk_done_seq++;
    }
    lat = walk_lat;
    lon = walk_lon;
  }
  lat_e7 = (int32_t)(lat * 1e7);
  lon_e7 = (int32_t)(lon * 1e7);
  t = DUMMY_EPOCH + (uint32_t)seq * 60;
#endif

  // fix 獲得遷移（no-fix → fix）でピピッ。
  if (fix && !had_fix) beep_fix();
  had_fix = fix;

  // ページ送り。
  if (page_cycle) page = (page + 1) % 3;

  // HOME 確定（長押し）: 現在地を出発点として保存し、seq をリセットして即時共有。
  if (home_confirm) {
    if (fix) {
      home_set = true;
      home_lat = lat;
      home_lon = lon;
      seq = 0;                 // 新しい行程の開始（受信側は HOME 変更で Trail リセット）
      have_last = false;
      course_deg_v = -1.0;
      beep_home();
      led_event(40, 40, 40, 1000);   // 白 1 秒＝HOME 確定
      Serial.printf("nostos-beacon: HOME set lat=%.7f lon=%.7f\n", home_lat, home_lon);
      if (send_home_frame(t, "home")) {
        last_tx_ms = now;
      }
    } else {
      beep_busy();             // fix なしでは確定できない
      Serial.println("nostos-beacon: HOME reject (no fix)");
    }
  }

  double moved = (have_last && fix) ? dist_m(last_tx_lat, last_tx_lon, lat, lon) : 0.0;
  bool moving = fix && have_last && moved >= MOVE_THRESHOLD_M;
  // 進路（course made good）: 前回送信位置から 10m 以上動いたら更新。
  if (fix && have_last && moved >= 10.0) {
    course_deg_v = bearing_to(last_tx_lat, last_tx_lon, lat, lon);
  }

  // 送信要否の判定
  const char *why = nullptr;
  if (manual) {
    why = "button";                                             // 任意発信（ダブルクリック）
  } else if (now < retry_after_ms) {
    why = nullptr;                                              // LBT 全滅後のバックオフ中
  } else if (fix && moving && (now - last_tx_ms) >= MIN_INTERVAL_MS) {
    why = "move";                                              // 移動 → 短間隔
  } else if (last_tx_ms == 0 || (now - last_tx_ms) >= MAX_INTERVAL_MS) {
    why = "heartbeat";                                        // 停止/初回 → 広間隔
  }

  if (why) {
    if (send_frame(fix ? NOSTOS_FLAG_FIX_VALID : 0, lat_e7, lon_e7, t, why)) {
      last_tx_ms = now;
      if (fix) { last_tx_lat = lat; last_tx_lon = lon; have_last = true; }
      // HOME 再放送: 通常送信 HOME_REBROADCAST_EVERY 回ごとに保存座標を FLAG_HOME で送る。
      if (home_set && ++tx_since_home >= HOME_REBROADCAST_EVERY) {
        send_home_frame(t, "home-rebcast");
      }
    } else {
      retry_after_ms = now + LBT_BACKOFF_MS;   // 混雑 → 少し待って再試行
    }
  }

  // ---- 表示・通知の定期更新 ----
  led_task(fix);
  static uint32_t last_oled_ms = 0;
  if (page_cycle || home_confirm || now - last_oled_ms >= 500) {
    last_oled_ms = now;
    oled_draw(fix, lat, lon, t, now);
  }
}
