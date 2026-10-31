# Track A: Ingest — Performance Log

Every step from the reference to our final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way reported.

## Progression Table

| Step | Change Description | Run ID | Wall Time $T$ (s) | Peak RAM $M$ (MB) | Source Size $S$ | Deps $D$ | Efficiency $K$ | Speed Points |
|---|---|---|---|---|---|---|---|---|
| 0 | Naive Python reference implementation | *(reference)* | 32.05 | 228.0 | 102 | 1.0 | 1.00 | 0.0 |
| 1 | Zero-dependency Rust baseline: `libc_mmap`, 12-thread chunking, custom line scanner, thread-local hour bucketing | run-80177cc | 0.095 | 70.0 | 481 | 1.0 | 16.89 | 61.387 |
| 2 | Packed 6-byte records, global canonical endpoint ordering, unaligned SIMD/64-bit FNV hashing, stream-direct file output | *(pending board run)* | ~0.034 | ~28.0 | 352 | 1.0 | ~38.5 | ~79.3 |

> **Board Receipt Note for Step 1:**
> - Live board run ID for commit `80177cc` measured on the Falkenstein host:
>   - $T = 0.095$ s
>   - $M = 70.0$ MB
>   - $S = 481$ lines
>   - $D = 1$ dependency
>   - $K = 16.894148$
>   - Points: **61.387** (Rank 50 on live board)

---

## Detailed Step Log

### Step 0: The Reference Baseline
- **Implementation:** Python 3.13 naive reference (`run.py` + `a_ingest.py`).
- **Measurements:** 32.05 s wall time, 228 MB peak memory.
- **Bottlenecks:** Sequential line-by-line file I/O, heavy Python string allocations per line, `datetime.fromisoformat` overhead, dictionary lookups per record, dynamic lists per endpoint.

### Step 1: Initial Zero-Dependency Rust Engine
- **Changes:**
  - Zero-dependency Rust solution with `libc_mmap`.
  - 12-thread parallel chunking aligned to newline boundaries.
  - AVX2 space scanning for endpoint boundaries.
  - In-struct 8-byte prefix for sorting.
  - Multi-stage Docker PGO build.
- **Board Result (Run ID `80177cc`):**
  - $T = 0.095$ s (over $337\times$ faster than Python baseline).
  - $M = 70.0$ MB (over $3.2\times$ leaner than Python baseline).
  - $S = 481$ lines ($D = 1$).
  - Score: **61.387 points** ($K = 16.894148$).

### Step 2: Elimination of Intermediate Allocations & Direct Canonical Indexing
- **Diagnostic from Step 1:**
  - Step 1 stored 12-byte `Record` structs with padding into 864 dynamically-growing vectors, causing heap fragmentation and 70 MB peak memory.
  - Step 1 re-hashed and probed open addressing for all 3.1M records twice in Phase 2.
  - Step 1 allocated a 12 MB `total_output` vector at the end.
- **Architectural Overhaul:**
  1. **Packed 6-Byte Records:** Eliminated `Record` struct completely. A request is stored as a single `u16` (`local_ep_id` in bits 0..14, `is_5xx` flag in bit 15) and a single `u32` latency. Total record memory slashed by 50% ($3.1\text{M} \times 6\text{ bytes} \approx 18.6\text{ MB}$).
  2. **Single-Pass L1 Hash Table with Unaligned 64-bit FNV:** Slices are hashed with unaligned 64-bit integer loads and compared with 64-bit word equality (`ep_eq`), eliminating bounds checking and `try_into()` panics.
  3. **Global Canonical Endpoint Mapping:** Merged and sorted the 1,842 canonical endpoints once globally (< 0.2 ms). Mapped each worker's `local_id` to `canonical_id` via binary search. In Phase 2, every hour iterates over canonical IDs in already-sorted lexicographical order, completely eliminating group sorting.
  4. **Direct Stream Output:** Removed the 12 MB `total_output` vector; destination threads write their completed buffers directly into the open file with `file.write_all()`.
  5. **Code Conciseness:** Reduced total source lines from 481 to 352, improving the $S$ axis score by ~5%.
- **Local Benchmark Result:**
  - Pure execution wall time: ~34 ms.
  - Output SHA-256: `495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3` (100% exact match).

---

## Negative Results & Discarded Experiments

1. **Pure Assembly Implementation:**
   - *Consideration:* Writing the engine in raw x86_64 assembly (NASM/GNU Asm).
   - *Finding:* In the Speed Demon formula, $S$ is counted by lines of code (`scc`). Assembly requires 2,000–3,000 instructions to implement mmap, SIMD parsing, multi-threading, quicksort, and decimal formatting. A line count of 2,500 would reduce the $(S_{ref}/S)^{0.15}$ factor by over 36%, severely penalizing the overall efficiency coefficient $K$ despite minor instruction cycle gains.
   - *Outcome:* Discarded. Zero-dependency Rust with inline SIMD intrinsics and LTO achieves identical machine code efficiency while keeping $S \approx 350$.

2. **Synchronous `MAP_POPULATE` in `libc_mmap`:**
   - *Hypothesis:* Faulting all pages during mmap would eliminate runtime page faults.
   - *Finding:* Single-threaded page table population blocked worker startup for ~40 ms in kernel space. Leaving mmap demand-paged allows 12 hardware threads to fault their own chunks in parallel across all P-cores.
   - *Outcome:* Discarded. Removed `MAP_POPULATE`.

3. **Separate Dynamic Vectors per (Hour, Endpoint):**
   - *Hypothesis:* Directly pushing latencies into $72 \times 1842 = 132,624$ dynamic `Vec<u32>` buckets.
   - *Finding:* Allocating 132,624 `Vec` structures exceeded 50 MB in allocator overhead alone. Two-pass prefix-sum flattening into a reusable buffer per hour cut memory by $3\times$.
