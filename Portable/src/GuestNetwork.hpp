#pragma once
#include "Base64.hpp"
namespace lt {
// A bounded upload to one of two fixed executable paths. The agent enforces
// sequential offsets and atomically renames the completed file. No shell/path
// supplied by the frontend is accepted.
inline std::string network_tool_chunk(const std::string &input, bool trust, std::string &body) {
    auto first = input.find(' '), second = input.find(' ', first == std::string::npos ? first : first + 1);
    require(first > 0 && first < 8 && second == first + 2, "Invalid network tool chunk");
    auto offset_text = input.substr(0, first);
    require(offset_text.find_first_not_of("0123456789") == std::string::npos &&
                (input[first + 1] == '0' || input[first + 1] == '1'), "Invalid network tool chunk");
    auto offset = std::stoul(offset_text);
    body = base64_decode(input.substr(second + 1));
    require(!body.empty() && body.size() <= 2048 && offset + body.size() <= 1024 * 1024,
            "Network tool exceeds upload limit");
    return std::to_string(offset) + " " + input[first + 1] + " 755 /tmp/lighttouch-" + (trust ? "ittrust" : "itproxy");
}
} // namespace lt
