// The fake cellular network a board with a modem offers (qemu-ios hw/misc/ios_baseband.c): a device's settings, saved
// as carrier.json in its record, applied at every boot as -global ios-baseband.<property>=<value> and written to a
// running modem when they change. The rules are the modem's own (ios_baseband_core.c: ios_bb_carrier_ok,
// ios_bb_plmn_ok, ios_bb_sms_sender_ok, ios_bb_gps_set, sms_build_deliver), so a value accepted here is one the modem
// accepts. The defaults are the 3GPP test network (MCC/MNC 001/01), as Light Touch for Mac's CarrierSettings.
#pragma once
#include "Internal.hpp"
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <set>
#include <string>
#include <utility>
#include <vector>

namespace lt::carrier {

struct Settings {
    std::string carrier = "Light Touch";
    std::string mcc_mnc = "00101";  // MCC (3 digits) + MNC (2 or 3)
    bool registered = true, sim_present = true;
    int bars = 5;  // 0-5
    // Where a modem with a GPS receiver (the 3GS's) says the phone is; every modem takes it.
    double latitude = 37.3349, longitude = -122.0090;
    // The numbers the app's Call... and Send text... last used.
    std::string last_caller = "+15555550100", last_sender = "+15555550100";
};

// Boards with a modem: the iPhone 3GS (n88), iPhone 4 (n90) and iPhone 5 (n42, its QMI modem on HSIC) boot it
// with baseband=on; the original iPhone's machine (m68) has it built in.
inline bool board_has_modem(const std::string &board) {
    return board == "n88ap" || board == "n90ap" || board == "n42ap" || board == "m68ap";
}

inline bool utf8_ok(const std::string &s) {
    for (size_t i = 0; i < s.size();) {
        const auto c = static_cast<unsigned char>(s[i]);
        const size_t n = c < 0x80 ? 1 : (c >> 5) == 6 ? 2 : (c >> 4) == 14 ? 3 : (c >> 3) == 30 ? 4 : 0;
        if (!n || i + n > s.size())
            return false;
        uint32_t v = n == 1 ? c : c & (0x7f >> n);
        for (size_t k = 1; k < n; k++) {
            const auto b = static_cast<unsigned char>(s[i + k]);
            if ((b & 0xc0) != 0x80)
                return false;
            v = v << 6 | (b & 0x3f);
        }
        static const uint32_t least[] = {0, 0, 0x80, 0x800, 0x10000};
        if (v < least[n] || v > 0x10ffff || (v >= 0xd800 && v <= 0xdfff))
            return false;
        i += n;
    }
    return true;
}

// 1-32 bytes, no control character or '"' (it goes out quoted in +COPS/+XCOPS).
inline bool carrier_ok(const std::string &s) {
    if (s.empty() || s.size() > 32 || !utf8_ok(s))
        return false;
    for (unsigned char c : s)
        if (c < 0x20 || c == 0x7f || c == '"')
            return false;
    return true;
}

// 5 or 6 digits: MCC + a 2- or 3-digit MNC.
inline bool plmn_ok(const std::string &s) {
    return (s.size() == 5 || s.size() == 6) && s.find_first_not_of("0123456789") == std::string::npos;
}

// A caller or an SMS sender: 1-20 digits, optionally after a '+'.
inline bool number_ok(const std::string &s) {
    const std::string digits = !s.empty() && s[0] == '+' ? s.substr(1) : s;
    return !digits.empty() && digits.size() <= 20 && digits.find_first_not_of("0123456789") == std::string::npos;
}

inline bool location_ok(double latitude, double longitude) {
    return std::isfinite(latitude) && std::isfinite(longitude) && std::fabs(latitude) <= 90 &&
           std::fabs(longitude) <= 180;
}

// How much of one SMS-DELIVER a text takes, as the modem packs it: GSM 7-bit septets (an escaped character counts
// twice) out of 160 when every character is in the default alphabet, else UTF-16 units out of 70. limit is 0 for
// text the modem can't send (characters outside the Basic Multilingual Plane, which it would turn into '?').
inline uint32_t sms_length(const std::string &text, uint32_t *limit) {
    // ios_baseband_core.c gsm7_main and gsm7_ext, as UTF-8.
    static const std::set<std::string> single = {
        "@", "\xc2\xa3", "$", "\xc2\xa5", "\xc3\xa8", "\xc3\xa9", "\xc3\xb9", "\xc3\xac", "\xc3\xb2", "\xc3\x87",
        "\n", "\xc3\x98", "\xc3\xb8", "\r", "\xc3\x85", "\xc3\xa5", "\xce\x94", "_", "\xce\xa6", "\xce\x93",
        "\xce\x9b", "\xce\xa9", "\xce\xa0", "\xce\xa8", "\xce\xa3", "\xce\x98", "\xce\x9e", "\xc3\x86", "\xc3\xa6",
        "\xc3\x9f", "\xc3\x89", " ", "!", "\"", "#", "\xc2\xa4", "%", "&", "'", "(", ")", "*", "+", ",", "-", ".",
        "/", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", ":", ";", "<", "=", ">", "?", "\xc2\xa1", "A", "B",
        "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X",
        "Y", "Z", "\xc3\x84", "\xc3\x96", "\xc3\x91", "\xc2\xa7", "\xc2\xbf", "\xc2\xa0", "a", "b", "c", "d", "e",
        "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
        "\xc3\xa4", "\xc3\xb6", "\xc3\xbc", "\xc3\xb1", "\xc3\xa0"};
    static const std::set<std::string> escaped = {"^", "{", "}", "\\", "[", "]", "|", "\xe2\x82\xac"};
    uint32_t septets = 0, units = 0;
    bool gsm7 = true;
    for (size_t i = 0; i < text.size();) {
        const auto c = static_cast<unsigned char>(text[i]);
        const size_t n = c < 0x80 ? 1 : (c >> 5) == 6 ? 2 : (c >> 4) == 14 ? 3 : 4;
        const std::string ch = text.substr(i, n);
        if (n == 4) {
            *limit = 0;
            return uint32_t(text.size());
        }
        units++;
        if (single.count(ch))
            septets++;
        else if (escaped.count(ch))
            septets += 2;
        else
            gsm7 = false;
        i += n;
    }
    *limit = gsm7 ? 160 : 70;
    return gsm7 ? septets : units;
}

inline bool sms_text_ok(const std::string &text) {
    uint32_t limit = 0;
    const uint32_t used = sms_length(text, &limit);
    return !text.empty() && utf8_ok(text) && used <= limit;
}

// The modem reports rssi = (dBm + 113) / 2, 0..31; iOS draws its bars from rssi.
inline int signal_dbm(int bars) {
    static const int dbm[] = {-113, -103, -97, -89, -81, -63};
    return dbm[bars < 0 ? 0 : bars > 5 ? 5 : bars];
}

// The modem's gps-fix: "lat,lon,altitude,speed,course,accuracy" (meters, m/s, degrees or -1 for none, meters).
inline std::string fix(double latitude, double longitude, double speed = 0, double course = -1) {
    char out[96];
    snprintf(out, sizeof out, "%.6f,%.6f,0,%.2f,%.1f,5", latitude, longitude, speed, course);
    return out;
}

// Why the settings aren't ones the modem takes, or "" when they are.
inline std::string problem(const Settings &s) {
    if (!carrier_ok(s.carrier))
        return "A carrier name is 1 to 32 characters without quotes";
    if (!plmn_ok(s.mcc_mnc))
        return "The MCC is 3 digits and the MNC 2 or 3";
    if (s.bars < 0 || s.bars > 5)
        return "Signal is 0 to 5 bars";
    if (!location_ok(s.latitude, s.longitude))
        return "Latitude is -90 to 90 and longitude -180 to 180 degrees";
    if (!s.last_caller.empty() && !number_ok(s.last_caller))
        return "A caller is 1 to 20 digits, optionally after a +";
    if (!s.last_sender.empty() && !number_ok(s.last_sender))
        return "A sender is 1 to 20 digits, optionally after a +";
    return "";
}

// The modem's properties for these settings, in the order a boot applies them.
inline std::vector<std::pair<std::string, std::string>> properties(const Settings &s) {
    return {{"carrier", s.carrier},
            {"mcc-mnc", s.mcc_mnc},
            {"registered", s.registered ? "on" : "off"},
            {"sim-present", s.sim_present ? "on" : "off"},
            {"signal-dbm", std::to_string(signal_dbm(s.bars))},
            {"gps-fix", fix(s.latitude, s.longitude)}};
}

// `-global` arguments that start the modem with these settings. Each is one argv element, and -global takes everything
// after the first '=' as the value verbatim (no comma escaping, unlike -M), so names and fixes with commas pass as is.
inline std::vector<std::string> globals(const Settings &s) {
    std::vector<std::string> out;
    for (const auto &p : properties(s)) {
        out.push_back("-global");
        out.push_back("ios-baseband." + p.first + "=" + p.second);
    }
    return out;
}

inline std::string json_string(const std::string &s) {
    Json j;
    j.kind = Json::String;
    j.text = s;
    return dump(j);
}

inline std::string to_json(const Settings &s) {
    char location[96];
    snprintf(location, sizeof location, "%.7f,\"longitude\":%.7f", s.latitude, s.longitude);
    return std::string("{\"schema\":1,\"carrier\":") + json_string(s.carrier) +
           ",\"mcc_mnc\":" + json_string(s.mcc_mnc) + ",\"registered\":" + (s.registered ? "true" : "false") +
           ",\"sim_present\":" + (s.sim_present ? "true" : "false") + ",\"bars\":" + std::to_string(s.bars) +
           ",\"latitude\":" + location + ",\"last_caller\":" + json_string(s.last_caller) + ",\"last_sender\":" + json_string(s.last_sender) +
           "}\n";
}

// The saved settings, or the defaults where there are none or they don't parse or validate.
inline Settings load(const fs::path &path) {
    Settings s;
    if (path.empty() || !fs::is_regular_file(path))
        return s;
    try {
        const Json j = parse(read_file(path, 16384));
        auto text = [&](const char *key, std::string &out) {
            if (const Json *v = j.find(key); v && v->kind == Json::String)
                out = v->text;
        };
        auto flag = [&](const char *key, bool &out) {
            if (const Json *v = j.find(key); v && v->kind == Json::Boolean)
                out = v->text == "true";
        };
        auto number = [&](const char *key, double &out) {
            if (const Json *v = j.find(key); v && v->kind == Json::Number)
                out = std::strtod(v->text.c_str(), nullptr);
        };
        text("carrier", s.carrier);
        text("mcc_mnc", s.mcc_mnc);
        flag("registered", s.registered);
        flag("sim_present", s.sim_present);
        double bars = s.bars;
        number("bars", bars);
        s.bars = bars >= 0 && bars <= 5 ? int(bars) : -1;
        number("latitude", s.latitude);
        number("longitude", s.longitude);
        text("last_caller", s.last_caller);
        text("last_sender", s.last_sender);
    } catch (const std::exception &) {
        return Settings{};
    }
    return problem(s).empty() ? s : Settings{};
}

} // namespace lt::carrier

namespace lt::carrier {

inline Settings from_abi(const lt_carrier *c) {
    abi(c);
    auto field = [](const char *s, size_t n) {
        const size_t len = strnlen(s, n);
        require(len < n, "carrier field exceeds ABI bound");
        return std::string(s, len);
    };
    Settings s;
    s.carrier = field(c->carrier, sizeof c->carrier);
    s.mcc_mnc = field(c->mcc_mnc, sizeof c->mcc_mnc);
    s.registered = c->registered != 0;
    s.sim_present = c->sim_present != 0;
    s.bars = c->bars > 5 ? -1 : int(c->bars);
    s.latitude = c->latitude;
    s.longitude = c->longitude;
    s.last_caller = field(c->last_caller, sizeof c->last_caller);
    s.last_sender = field(c->last_sender, sizeof c->last_sender);
    return s;
}

inline void to_abi(const Settings &s, lt_carrier *c) {
    abi(c);
    auto field = [](char *out, size_t n, const std::string &v) { snprintf(out, n, "%s", v.c_str()); };
    field(c->carrier, sizeof c->carrier, s.carrier);
    field(c->mcc_mnc, sizeof c->mcc_mnc, s.mcc_mnc);
    c->registered = s.registered;
    c->sim_present = s.sim_present;
    c->bars = uint32_t(s.bars);
    c->latitude = s.latitude;
    c->longitude = s.longitude;
    field(c->last_caller, sizeof c->last_caller, s.last_caller);
    field(c->last_sender, sizeof c->last_sender, s.last_sender);
}

} // namespace lt::carrier
