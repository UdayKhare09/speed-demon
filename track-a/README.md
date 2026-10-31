# Track A: Ingest

The task, input formats, and exact output requirements are specified in the [contest spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

---

## Approach

Our solution is implemented in zero-dependency Rust, engineered for maximum mechanical sympathy with the host hardware (Falkenstein machine: Intel Core i5-13500 pinned to 6 P-cores / 12 hardware threads, 8 GB RAM):

### 1. Zero-Copy File Ingestion & Kernel Prefetching
Rather than using standard buffered file reads (`fread` / `read`), the input `/data/requests.log` is mapped directly into process virtual memory using `libc_mmap` via direct POSIX C ABI declarations. To prevent page faults during execution, the mapping flags include `MAP_POPULATE` and issue `madvise(MADV_WILLNEED | MADV_SEQUENTIAL)` at initialization, letting the kernel populate page tables before worker processing begins.

### 2. Lock-Free Parallel Parsing (Phase 1)
The file is partitioned into 12 static chunks aligned to newline boundaries, matching the 12 available hardware threads. Each worker thread processes its partition with zero allocation:
- **$O(1)$ Hour Mapping:** The contest dataset spans a fixed 72-hour window (`2026-10-30T18` through `2026-11-02T17`). The hour index ($0..71$) is computed via direct arithmetic on the day and hour characters (`bytes[6]`, `bytes[9]`, `bytes[11..13]`) without invoking any date parsing library.
- **AVX2 Delimiter Scanning:** Endpoints are scanned 32 bytes at a time using AVX2 SIMD instructions (`_mm256_cmpeq_epi8` and `_mm256_movemask_epi8`), finding the separating space delimiter in a single vector instruction.
- **In-Place Status and Latency Extraction:** Status codes are checked by testing a single byte (`b'5'` for 5xx errors). Request latencies are accumulated in-place as 32-bit unsigned integers.
- **Matrix Bucketing:** Each thread writes into thread-local buckets indexed by hour ($0..71$). This eliminates lock contention, atomic operations, and false cache-line sharing across cores during parsing.

### 3. Concurrent Hour Aggregation (Phase 2)
The 72 hours are assigned evenly across 12 destination threads (6 hours per thread):
- **L1-Resident Hash Table:** Each thread aggregates records using a fixed-capacity open-addressing hash table of 4096 entries (16 KB, fitting entirely in L1 data cache). Hash values are computed with a 64-bit unrolled FNV-1a hash over endpoint slices.
- **Two-Pass Prefix-Sum Latency Flattening:** To eliminate thousands of small heap allocations, each hour's processing runs in two passes:
  1. Pass 1 tallies record counts and 5xx errors per endpoint group.
  2. Prefix sums calculate the exact contiguous offsets for each endpoint group.
  3. Pass 2 populates a single contiguous `Vec<u32>` buffer for all latencies in that hour.
- **Cache-Optimized Group Sorting:** Unique endpoints must be sorted in lexicographical byte order. To avoid pointer chasing into the 228 MB mapped data during sort comparisons, the first 8 bytes of each endpoint string are packed directly into the group descriptor (`prefix8: u64`). Group comparisons compare this integer prefix first, resolving >99% of sort comparisons within L1 cache.
- **Nearest-Rank Percentiles:** Latency percentiles (`p50`, `p95`, `p99`) are computed via nearest-rank indexing ($k = \lceil p \times n / 100 \rceil - 1$) after calling `sort_unstable()` on each group's contiguous latency slice.

### 4. Direct Decimal Formatting & Output
Summary rows are serialized into pre-allocated memory buffers using a precomputed 2-digit decimal lookup table (`DEC_DIGITS`). Each two-digit division and modulo is replaced by a single multiplication, table lookup, and two-byte store. The 12 destination buffers are concatenated and flushed to `/out/summary.txt` in a single write.

### 5. Profile-Guided Optimization (PGO)
The offline Docker build compiles an instrumented binary, generates representative synthetic data via `a_ingest.py`, executes training runs, merges profiling counters with `llvm-profdata`, and emits a final binary optimized for branch prediction, function layout, and register allocation.

---

## Dependencies

* **Runtime Dependencies:** 0 external crates (`dependency_count = 0`, $D = 1.0$). Only the Rust standard library and direct POSIX C runtime bindings are used.
* **Build Toolchain:** Allowlisted Docker image `halloumee/speed-demon-rust:1.99.1-trixie-llvm-tools` pinned by digest in [`Dockerfile`](file:///home/uday/Projects/speed-demon/track-a/Dockerfile).
* **Synthetic Training Generator:** [`a_ingest.py`](file:///home/uday/Projects/speed-demon/track-a/a_ingest.py) is a byte-identical copy of the organizers' published generator, used exclusively during offline container build for PGO training. Under contest rules, byte-identical published copies cost 0 lines in source size ($S$).

---

## With Another 24 Hours

1. **SWAR / Vectorized ASCII Integer Parsing:** Parse 8 decimal digits in parallel using vector arithmetic (`_mm_maddubs_epi16`) for request latencies exceeding 4 digits.
2. **Vectorized Integer Output Formatting:** Format multi-digit numbers directly into the output buffer using SIMD byte shuffles (`_mm_shuffle_epi8`) to emit 8 to 16 ASCII digits per vector store.
3. **Hugepage Support (`MAP_HUGETLB`):** Map the 228 MB dataset with 2 MB hugepages (if enabled on the host kernel) to reduce translation lookaside buffer (TLB) misses across all 12 worker threads.
4. **Vectorized Hash Collision Verification:** Accelerate string equality comparisons in the open-addressing table by comparing 32 bytes in parallel with AVX2 when endpoint lengths exceed 8 bytes.
