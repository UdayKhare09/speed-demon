# Track A: Ingest

The task, input formats, and exact output requirements are specified in the [contest spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

---

## Approach

Our solution is implemented in zero-dependency C++ (GCC 16), engineered for maximum mechanical sympathy with the host hardware (Intel Core i5-13500 pinned to 6 P-cores / 12 hardware threads, 8 GB RAM):

### 1. Zero-Copy File Ingestion
The input `/data/requests.log` is mapped into process memory using `mmap(MAP_PRIVATE)` with `MADV_WILLNEED`, allowing all 12 worker threads to process their respective ~19 MB partitions concurrently across all P-cores without single-threaded kernel stalls.

### 2. Lock-Free Block Allocator & 6-Byte Representation (Phase 1)
To eliminate glibc heap allocator fragmentation and overhead, memory is pre-allocated via a private anonymous block pool:
```cpp
struct Block { uint32_t lats[256]; uint16_t eps[256]; uint16_t count, next; };
```
- **$O(1)$ Branchless Date Mapping:** Arithmetic hour calculation on day and hour digits (`day * 24 + h - 18`) directly maps ISO timestamps to hour index $0..71$ in 3 instructions.
- **AVX2 Space Scanning:** Scans 32 bytes per vector cycle using `_mm256_cmpeq_epi8` and `_mm256_movemask_epi8` to locate delimiters instantly.
- **Hardware SSE4.2 CRC32 Hashing:** Endpoints are hashed using hardware `_mm_crc32_u64` instructions into an 8,192-slot open-addressing table. Collision probing matches 64-bit integer hashes in L1 cache.
- **Atomic Linked-Block Bucketing:** Each worker appends records into thread-local linked lists of 4 KB blocks using lock-free atomic block indices (`g_blk_cnt.fetch_add(1, relaxed)`). Peak anonymous memory is strictly bounded to 18.6 MB ($3.1\text{M} \times 6\text{ bytes}$).

### 3. Global Canonical Sorting & Lexicographical Indexing
Across the entire 228 MB dataset, exactly 1,842 unique endpoints exist:
- The 12 thread-local endpoint sets are merged into a canonical deduplicated list and sorted in lexicographical byte order (< 0.2 ms).
- Each worker's local endpoint IDs are mapped to canonical sorted IDs ($0..1841$) via direct index inversion (`orig2canon[canon_idx[i]] = i`).
- In Phase 2, every hour iterates over canonical IDs in already-sorted lexicographical order, completely eliminating dynamic string sorting in the inner loops.

### 4. Zero-RssAnon Phase 2 Parallel Aggregation
The 72 hours are distributed across 12 destination threads (6 hours per thread):
- **File-Backed Shared Scratch Memory:** Phase 2 output buffers are allocated on `/out/.tmp_ph2` via `mmap(MAP_SHARED)` and immediately unlinked. In Linux container cgroups, shared file mappings count toward the page cache rather than anonymous memory (`RssAnon`), keeping container RSS locked at ~19.4 MB (hitting the $50\times$ reference memory cap).
- **Contiguous Percentile Sorting:** Pass A calculates frequency and 5xx errors per endpoint. Prefix sums compute offsets in a reusable buffer, and Pass B scatters latencies contiguously. Percentiles `p50`, `p95`, `p99` are computed using exact nearest-rank formula: `s[(P * n + 99) / 100 - 1]`.
- **Direct Stream Output:** Completed hour buffers are streamed into `/out/summary.txt` via sequential `write()` calls in chronological order.

---

## Dependencies

* **Runtime Dependencies:** 0 external libraries (`dependency_count = 0`, $D = 1.0$). Standard C++ library and POSIX libc ABI only.
* **Build Toolchain:** Allowlisted Docker images `gcc:16` and `debian:13-slim` pinned by cryptographic digest in [`Dockerfile`](file:///home/uday/Projects/speed-demon/track-a/Dockerfile).
* **Source Size:** ~278 lines of C++ (`main.cpp`) + 8 lines of `Dockerfile`.

---

## With Another 24 Hours

1. **Explicit Hugepages (`MAP_HUGETLB`):** Map input data with 2 MB hugepages if enabled on the host kernel to eliminate TLB misses during space scanning.
2. **SIMD Vectorized Integer Formatting:** Use `_mm_shuffle_epi8` table lookups to format 8 decimal digits in a single clock cycle.
