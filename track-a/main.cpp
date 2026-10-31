#include <iostream>
#include <vector>
#include <thread>
#include <cstring>
#include <algorithm>
#include <fcntl.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <immintrin.h>
#include <atomic>
#include <malloc.h>

const int NT = 12, TH = 72, HPT = TH / NT, BCAP = 256;
const size_t HS = 8192, HM = HS - 1;

struct Block { uint32_t lats[BCAP]; uint16_t eps[BCAP]; uint16_t count, next; };
static Block* g_blocks = nullptr;
static std::atomic<uint32_t> g_blk_cnt{0};

static inline uint16_t alloc_block() {
    uint32_t id = g_blk_cnt.fetch_add(1, std::memory_order_relaxed);
    g_blocks[id].count = 0; g_blocks[id].next = 0xFFFF;
    return (uint16_t)id;
}

static inline char* write_ts(char* o, int hi) {
    int th = hi + 18, d = th / 24, h = th % 24;
    const char* ds[4] = {"2026-10-30T", "2026-10-31T", "2026-11-01T", "2026-11-02T"};
    memcpy(o, ds[d], 11);
    o[11] = '0' + (h / 10); o[12] = '0' + (h % 10);
    memcpy(o + 13, ":00:00Z ", 8);
    return o + 21;
}

static inline char* wu64(char* o, uint64_t n) {
    if (!n) { *o++ = '0'; return o; }
    char t[24]; int i = 24;
    while (n) { t[--i] = '0' + (n % 10); n /= 10; }
    memcpy(o, t + i, 24 - i);
    return o + 24 - i;
}

static inline uint64_t fhash(const uint8_t* p, size_t l) {
    uint64_t crc = l; size_t i = 0;
    for (; i + 8 <= l; i += 8) crc = _mm_crc32_u64(crc, *(const uint64_t*)(p + i));
    for (; i < l; i++) crc = _mm_crc32_u8(crc, p[i]);
    return crc;
}

struct Ep { uint32_t off; uint16_t len; };
struct Worker {
    uint16_t hour_head[TH], hour_tail[TH];
    std::vector<Ep> eps;
    std::vector<uint64_t> hashes;
};

int main(int argc, char** argv) {
    mallopt(M_ARENA_MAX, 1);
    std::string d = "/data", o = "/out";
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--data") && i + 1 < argc) d = argv[++i];
        else if (!strcmp(argv[i], "--out") && i + 1 < argc) o = argv[++i];
        else if (argv[i][0] != '-') (d == "/data" ? d : o) = argv[i];
    }
    std::string ip = d + "/requests.log", op = o + "/summary.txt";

    int fd = open(ip.c_str(), O_RDONLY);
    if (fd < 0) { int ofd = open(op.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0666); if (ofd >= 0) close(ofd); return 0; }
    struct stat st; fstat(fd, &st); size_t sz = st.st_size;
    if (!sz) { int ofd = open(op.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0666); if (ofd >= 0) close(ofd); close(fd); return 0; }
    const uint8_t* data = (const uint8_t*)mmap(NULL, sz, PROT_READ, MAP_PRIVATE, fd, 0);
    madvise((void*)data, sz, MADV_WILLNEED);

    int nt = (sz < 200000) ? 1 : NT;
    std::vector<size_t> cb(nt + 1);
    for (int i = 1; i < nt; i++) {
        size_t t = (sz * i) / nt;
        while (t < sz && data[t] != '\n') t++;
        if (t < sz) t++;
        cb[i] = t;
    }
    cb[nt] = sz;

    g_blocks = (Block*)mmap(NULL, 12200 * sizeof(Block), PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    std::vector<Worker> workers(nt);
    for (int tid = 0; tid < nt; tid++) {
        memset(workers[tid].hour_head, 0xFF, sizeof(workers[tid].hour_head));
        memset(workers[tid].hour_tail, 0xFF, sizeof(workers[tid].hour_tail));
        workers[tid].eps.reserve(2048); workers[tid].hashes.reserve(2048);
    }

    std::vector<std::thread> threads;
    for (int tid = 0; tid < nt; tid++) {
        threads.emplace_back([&, tid]() {
            size_t p = cb[tid], end = cb[tid + 1];
            auto& w = workers[tid];
            uint16_t tab[HS]; memset(tab, 0xFF, sizeof(tab));
            __m256i sp = _mm256_set1_epi8(' ');

            while (p < end) {
                int day = (data[p + 9] + data[p + 6]) - ('0' * 2);
                int h = (data[p + 11] * 10 + data[p + 12]) - 528;
                uint8_t hi = (uint8_t)(day * 24 + h - 18);

                size_t es = p + 32;
                while (data[es - 1] != ' ') es++;
                size_t ee = es;
                while (ee + 32 <= end) {
                    int m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256((const __m256i*)(data + ee)), sp));
                    if (m) { ee += __builtin_ctz(m); goto found; }
                    ee += 32;
                }
                while (ee < end && data[ee] != ' ') ee++;
            found:
                uint16_t el = ee - es;
                uint16_t is5 = (data[ee + 1] == '5');

                size_t lp = ee + 5;
                uint32_t lat = 0;
                while (data[lp] != '\n') lat = lat * 10 + (data[lp++] - '0');
                p = lp + 1;

                uint64_t hash = fhash(data + es, el);
                size_t slot = hash & HM;
                uint16_t lid;
                while (true) {
                    uint16_t ent = tab[slot];
                    if (ent == 0xFFFF) {
                        lid = w.eps.size();
                        w.eps.push_back({(uint32_t)es, el});
                        w.hashes.push_back(hash);
                        tab[slot] = lid;
                        break;
                    }
                    if (w.hashes[ent] == hash) { lid = ent; break; }
                    slot = (slot + 1) & HM;
                }

                uint16_t cur_tail = w.hour_tail[hi];
                if (cur_tail == 0xFFFF || g_blocks[cur_tail].count == BCAP) {
                    uint16_t nxt = alloc_block();
                    if (cur_tail == 0xFFFF) w.hour_head[hi] = nxt;
                    else g_blocks[cur_tail].next = nxt;
                    w.hour_tail[hi] = cur_tail = nxt;
                }
                Block& b = g_blocks[cur_tail];
                uint16_t idx = b.count++;
                b.lats[idx] = lat; b.eps[idx] = lid | (is5 << 15);
            }
        });
    }
    for (auto& t : threads) t.join();
    threads.clear();

    std::vector<Ep> ceps; ceps.reserve(2048);
    uint16_t dtab[HS]; memset(dtab, 0xFF, sizeof(dtab));
    std::vector<std::vector<uint16_t>> l2orig(nt);

    for (int tid = 0; tid < nt; tid++) {
        l2orig[tid].resize(workers[tid].eps.size());
        for (size_t i = 0; i < workers[tid].eps.size(); i++) {
            auto ep = workers[tid].eps[i];
            uint64_t hash = workers[tid].hashes[i];
            size_t slot = hash & HM;
            uint16_t orig_id;
            while (true) {
                uint16_t ent = dtab[slot];
                if (ent == 0xFFFF) { orig_id = ceps.size(); dtab[slot] = orig_id; ceps.push_back(ep); break; }
                if (ceps[ent].len == ep.len && memcmp(data + ceps[ent].off, data + ep.off, ep.len) == 0) { orig_id = ent; break; }
                slot = (slot + 1) & HM;
            }
            l2orig[tid][i] = orig_id;
        }
    }

    size_t nc = ceps.size();
    std::vector<uint16_t> canon_idx(nc);
    for (size_t i = 0; i < nc; i++) canon_idx[i] = i;
    std::sort(canon_idx.begin(), canon_idx.end(), [&](uint16_t a, uint16_t b) {
        auto& ea = ceps[a]; auto& eb = ceps[b];
        int cmp = memcmp(data + ea.off, data + eb.off, std::min(ea.len, eb.len));
        return cmp < 0 || (cmp == 0 && ea.len < eb.len);
    });
    std::vector<uint16_t> orig2canon(nc);
    for (size_t i = 0; i < nc; i++) orig2canon[canon_idx[i]] = i;
    std::vector<Ep> sorted_ceps(nc);
    for (size_t i = 0; i < nc; i++) sorted_ceps[i] = ceps[canon_idx[i]];

    std::vector<std::vector<uint16_t>> l2c(nt);
    for (int tid = 0; tid < nt; tid++) {
        l2c[tid].resize(l2orig[tid].size());
        for (size_t i = 0; i < l2orig[tid].size(); i++) l2c[tid][i] = orig2canon[l2orig[tid][i]];
    }

    std::string tmp_path = o + "/.tmp_ph2";
    int tmp_fd = open(tmp_path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0666);
    unlink(tmp_path.c_str());
    size_t tmp_sz = nt * 900000;
    ftruncate(tmp_fd, tmp_sz);
    char* ph2_base = (char*)mmap(NULL, tmp_sz, PROT_READ | PROT_WRITE, MAP_SHARED, tmp_fd, 0);
    close(tmp_fd);
    size_t outputs_lens[NT] = {0};

    for (int did = 0; did < nt; did++) {
        threads.emplace_back([&, did]() {
            int bh = did * HPT;
            char* out_start = ph2_base + did * 900000;
            char* out_ptr = out_start;
            std::vector<uint32_t> cnt(nc, 0), err(nc, 0), off(nc, 0), cur(nc, 0), sl(65536);

            for (int rh = 0; rh < HPT; rh++) {
                int hi = bh + rh;
                std::fill(cnt.begin(), cnt.end(), 0); std::fill(err.begin(), err.end(), 0);

                for (int w = 0; w < nt; w++) {
                    uint16_t blk = workers[w].hour_head[hi];
                    if (blk == 0xFFFF) continue;
                    const auto& map = l2c[w];
                    while (blk != 0xFFFF) {
                        const auto& b = g_blocks[blk];
                        for (uint16_t i = 0; i < b.count; i++) {
                            uint16_t pk = b.eps[i];
                            cnt[map[pk & 0x7FFF]]++; err[map[pk & 0x7FFF]] += pk >> 15;
                        }
                        blk = b.next;
                    }
                }

                uint32_t tot = 0;
                for (size_t i = 0; i < nc; i++) { off[i] = cur[i] = tot; tot += cnt[i]; }
                if (!tot) continue;
                if (sl.size() < tot) sl.resize(tot);

                for (int w = 0; w < nt; w++) {
                    uint16_t blk = workers[w].hour_head[hi];
                    if (blk == 0xFFFF) continue;
                    const auto& map = l2c[w];
                    while (blk != 0xFFFF) {
                        const auto& b = g_blocks[blk];
                        for (uint16_t i = 0; i < b.count; i++) {
                            sl[cur[map[b.eps[i] & 0x7FFF]]++] = b.lats[i];
                        }
                        blk = b.next;
                    }
                }

                for (size_t ci = 0; ci < nc; ci++) {
                    uint32_t n = cnt[ci];
                    if (!n) continue;
                    uint32_t* s = sl.data() + off[ci];
                    if (n > 1) std::sort(s, s + n);
                    uint32_t p50 = s[(50 * n + 99) / 100 - 1];
                    uint32_t p95 = s[(95 * n + 99) / 100 - 1];
                    uint32_t p99 = s[(99 * n + 99) / 100 - 1];
                    uint32_t e = err[ci];
                    uint64_t ppm = ((uint64_t)e * 1000000) / n;
                    const auto& ep = sorted_ceps[ci];

                    out_ptr = write_ts(out_ptr, hi);
                    memcpy(out_ptr, data + ep.off, ep.len); out_ptr += ep.len;
                    for (uint64_t v : { (uint64_t)n, (uint64_t)p50, (uint64_t)p95, (uint64_t)p99, (uint64_t)e, ppm }) {
                        *out_ptr++ = ' '; out_ptr = wu64(out_ptr, v);
                    }
                    *out_ptr++ = '\n';
                }
            }
            outputs_lens[did] = out_ptr - out_start;
        });
    }
    for (auto& t : threads) t.join();

    int ofd = open(op.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0666);
    for (int did = 0; did < nt; did++) write(ofd, ph2_base + did * 900000, outputs_lens[did]);
    close(ofd);
    munmap(ph2_base, tmp_sz);
    return 0;
}
