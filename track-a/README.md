# Track A: Ingest

The task, input formats, and exact output requirements are specified in the [contest spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

---

## Approach

Our solution is implemented in zero-dependency Rust, engineered for maximum mechanical sympathy with the host hardware (Falkenstein machine: Intel Core i5-13500 pinned to 6 P-cores / 12 hardware threads, 8 GB RAM):

### 1. Zero-Copy File Ingestion
The input `/data/requests.log` is mapped into process memory using `libc_mmap` with demand paging and `MADV_SEQUENTIAL`, allowing all 12 worker threads to fault their own 19 MB partitions concurrently across all P-cores without single-threaded kernel stalls.

### 2. Lock-Free Parallel Parsing & Packed 6-Byte Representation (Phase 1)
The file is partitioned into 12 static chunks aligned to newline boundaries. Each worker thread processes its partition with zero heap allocation:
- **$O(1)$ Date Mapping:** Arithmetic hour calculation on day and hour digits (`bytes[6]`, `bytes[9]`, `bytes[11..13]`) maps timestamps directly to hour index $0..71$.
- **AVX2 Space Scanning:** Scans 32 bytes per vector cycle using `_mm256_cmpeq_epi8` and `_mm256_movemask_epi8` to locate delimiters instantly.
- **L1-Resident Hash Table with 64-bit Word Comparisons:** Endpoints are hashed using unaligned 64-bit FNV-1a arithmetic into a 4096-slot open-addressing table. Collision checks compare 8 bytes per iteration (`ep_eq`) with unaligned integer reads, avoiding libc overhead.
- **Packed 6-Byte Storage:** Rather than storing multi-field structs with memory padding, requests are encoded as a single `u16` (`local_ep_id` in bits 0..14, `is_5xx` flag in bit 15) and a single `u32` latency. Total record memory across all 3.1M requests is constrained to ~18.6 MB.

### 3. Global Canonical Sorting & Lexicographical Indexing
Across the entire 228 MB dataset, exactly 1,842 unique endpoints exist. At the end of Phase 1:
- The 12 thread-local endpoint sets are merged into a canonical list of 1,842 unique endpoints and sorted in lexicographical byte order (< 0.2 ms).
- Each worker's local endpoint IDs are mapped to canonical sorted IDs ($0..1841$) via binary search.
- In Phase 2, every hour automatically iterates over endpoints in exact canonical sorted order, completely eliminating dynamic string sorting in the inner loops.

### 4. Direct Contiguous Aggregation (Phase 2)
The 72 hours are distributed across 12 destination threads (6 hours per thread):
- **Two-Pass Contiguous Flattening:** Pass A counts requests and errors per canonical endpoint. Prefix sums compute starting offsets in a reusable `sorted_lats` buffer. Pass C scatters latencies directly into contiguous slices.
- **In-Place Percentile Sorting:** `slice.sort_unstable()` sorts only the contiguous latency slice for each endpoint (median length is 6), calculating `p50`, `p95`, `p99` via nearest-rank formulas.
- **Stream Output:** Summary lines are formatted with precomputed 2-digit decimal lookup tables (`DEC_DIGITS`) and written directly to the output file sequentially, eliminating large concatenated buffer allocations.

### 5. Profile-Guided Optimization (PGO)
The offline Docker build generates synthetic data via byte-identical `a_ingest.py`, collects profile data with `-C profile-generate`, merges counters via `llvm-profdata`, and emits a final binary optimized for branch prediction, function layout, and register allocation.

---

## Dependencies

* **Runtime Dependencies:** 0 external crates (`dependency_count = 0`, $D = 1.0$). Standard library and POSIX libc ABI only.
* **Build Toolchain:** Allowlisted Docker image `halloumee/speed-demon-rust:1.99.1-trixie-llvm-tools` pinned by digest in [`Dockerfile`](file:///home/uday/Projects/speed-demon/track-a/Dockerfile).
* **Synthetic Training Generator:** [`a_ingest.py`](file:///home/uday/Projects/speed-demon/track-a/a_ingest.py) is a byte-identical copy of the organizers' published generator (costs 0 lines in $S$).

---

## With Another 24 Hours

1. **SWAR / SIMD Integer Parsing:** Parse 8 decimal digits in parallel using vector arithmetic (`_mm_maddubs_epi16`).
2. **Vectorized Integer Output Formatting:** Format multi-digit numbers directly into the output buffer using SIMD byte shuffles (`_mm_shuffle_epi8`).
3. **Hugepage Support (`MAP_HUGETLB`):** Map the 228 MB dataset with 2 MB hugepages if enabled on host.
