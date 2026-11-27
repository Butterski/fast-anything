// Reference C++ port of fuzzy.find_duplicates (with levenshtein and normalize inside it): what a
// good agent run produces. CI uses it to test the C++ pipeline without an agent.
#include "port.h"

#include <algorithm>
#include <thread>

namespace port {
namespace {

bool ascii(const std::u32string& s) {
    return std::all_of(s.begin(), s.end(), [](char32_t c) { return c < 0x80; });
}

// What str.split() splits on, among ASCII characters.
bool ascii_space(char32_t c) {
    return c == U' ' || (c >= 0x09 && c <= 0x0d) || (c >= 0x1c && c <= 0x1f);
}

// Python's " ".join(name.lower().split()), as code points; non-ASCII text goes through Python's own
// lower() and split().
std::u32string normalize(const std::u32string& name) {
    std::u32string out;
    if (!ascii(name)) {
        auto words = fa::str_method<std::vector<std::u32string>>(fa::str_method<std::u32string>(name, "lower"), "split");
        for (const auto& word : words) {
            if (!out.empty()) out += U' ';
            out += word;
        }
        return out;
    }
    out.reserve(name.size());
    bool gap = false;
    for (char32_t c : name) {
        if (ascii_space(c)) {
            gap = !out.empty();
            continue;
        }
        if (gap) {
            out += U' ';
            gap = false;
        }
        out += (c >= U'A' && c <= U'Z') ? c + (U'a' - U'A') : c;
    }
    return out;
}

// For each code point, the bit set of positions where it occurs in a pattern of up to 64.
class Masks {
public:
    explicit Masks(const std::u32string& pattern) {
        for (size_t i = 0; i < pattern.size(); ++i) {
            const uint64_t bit = uint64_t{1} << i;
            const char32_t c = pattern[i];
            if (c < 128) {
                ascii_[c] |= bit;
                continue;
            }
            auto it = std::find_if(other_.begin(), other_.end(), [c](const auto& e) { return e.first == c; });
            if (it == other_.end()) other_.emplace_back(c, bit); else it->second |= bit;
        }
    }
    uint64_t operator[](char32_t c) const {
        if (c < 128) return ascii_[c];
        for (const auto& [k, mask] : other_) if (k == c) return mask;
        return 0;
    }

private:
    uint64_t ascii_[128] = {};
    std::vector<std::pair<char32_t, uint64_t>> other_;
};

// Edit distance of a pattern of 1 to 64 code points against text: Hyyrö's bit-parallel form of
// Myers' algorithm, one 64-bit word per column of the table.
int64_t bit_parallel(const Masks& masks, size_t m, const std::u32string& text) {
    uint64_t vp = ~uint64_t{0}, vn = 0;
    const uint64_t last = uint64_t{1} << (m - 1);
    int64_t dist = static_cast<int64_t>(m);
    for (char32_t c : text) {
        const uint64_t x = masks[c];
        const uint64_t d0 = (((x & vp) + vp) ^ vp) | x | vn;
        uint64_t hp = vn | ~(d0 | vp);
        uint64_t hn = d0 & vp;
        dist += (hp & last) != 0;
        dist -= (hn & last) != 0;
        hp = (hp << 1) | 1;
        hn <<= 1;
        vp = hn | ~(d0 | hp);
        vn = hp & d0;
    }
    return dist;
}

// Edit distance by the plain two-row table, for patterns longer than 64.
int64_t table(const std::u32string& a, const std::u32string& b, std::vector<int64_t>& row) {
    row.resize(b.size() + 1);
    for (size_t j = 0; j <= b.size(); ++j) row[j] = static_cast<int64_t>(j);
    for (size_t i = 0; i < a.size(); ++i) {
        int64_t diagonal = row[0];
        row[0] = static_cast<int64_t>(i + 1);
        for (size_t j = 0; j < b.size(); ++j) {
            const int64_t above = row[j + 1];
            row[j + 1] = std::min({above + 1, row[j] + 1, diagonal + (a[i] != b[j])});
            diagonal = above;
        }
    }
    return row[b.size()];
}

// The pairs (i, j), i < j, of row i that are within max edits.
void row_pairs(const std::vector<std::u32string>& clean, size_t i, int64_t max, std::vector<int64_t>& scratch,
               std::vector<std::tuple<int64_t, int64_t>>& out) {
    const std::u32string& a = clean[i];
    const bool fits = !a.empty() && a.size() <= 64;
    const Masks masks = fits ? Masks(a) : Masks(std::u32string());
    for (size_t j = i + 1; j < clean.size(); ++j) {
        const std::u32string& b = clean[j];
        const int64_t diff = static_cast<int64_t>(a.size() > b.size() ? a.size() - b.size() : b.size() - a.size());
        if (diff > max) continue;  // the distance is at least the length difference
        int64_t dist;
        if (a.empty()) dist = static_cast<int64_t>(b.size());
        else if (fits) dist = bit_parallel(masks, a.size(), b);
        else dist = table(a, b, scratch);
        if (dist <= max) out.emplace_back(static_cast<int64_t>(i), static_cast<int64_t>(j));
    }
}

}  // namespace

std::vector<std::tuple<int64_t, int64_t>> find_duplicates(const std::vector<std::u32string>& names, int64_t max_distance) {
    std::vector<std::tuple<int64_t, int64_t>> found;
    if (max_distance < 0) return found;
    std::vector<std::u32string> clean;
    clean.reserve(names.size());
    for (const auto& name : names) clean.push_back(normalize(name));

    const size_t n = clean.size();
    const size_t work = n * (n > 0 ? n - 1 : 0) / 2;
    const size_t threads = work < 20000 ? 1 : std::max<size_t>(1, std::min<size_t>(std::thread::hardware_concurrency(), n));
    std::vector<std::vector<std::tuple<int64_t, int64_t>>> parts(threads);
    auto rows = [&](size_t t) {
        std::vector<int64_t> scratch;
        for (size_t i = t; i < n; i += threads) row_pairs(clean, i, max_distance, scratch, parts[t]);
    };
    if (threads == 1) {
        rows(0);
    } else {
        std::vector<std::thread> pool;
        for (size_t t = 0; t < threads; ++t) pool.emplace_back(rows, t);
        for (auto& th : pool) th.join();
    }
    for (auto& part : parts) found.insert(found.end(), part.begin(), part.end());
    std::sort(found.begin(), found.end());
    return found;
}

}  // namespace port
