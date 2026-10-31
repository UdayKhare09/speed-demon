use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::Arc;

const NUM_THREADS: usize = 12;
const TOTAL_HOURS: usize = 72;
const HOURS_PER_THREAD: usize = 6;

const HOURS_STR: [&[u8; 13]; 72] = [
    b"2026-10-30T18", b"2026-10-30T19", b"2026-10-30T20", b"2026-10-30T21", b"2026-10-30T22", b"2026-10-30T23",
    b"2026-10-31T00", b"2026-10-31T01", b"2026-10-31T02", b"2026-10-31T03", b"2026-10-31T04", b"2026-10-31T05",
    b"2026-10-31T06", b"2026-10-31T07", b"2026-10-31T08", b"2026-10-31T09", b"2026-10-31T10", b"2026-10-31T11",
    b"2026-10-31T12", b"2026-10-31T13", b"2026-10-31T14", b"2026-10-31T15", b"2026-10-31T16", b"2026-10-31T17",
    b"2026-10-31T18", b"2026-10-31T19", b"2026-10-31T20", b"2026-10-31T21", b"2026-10-31T22", b"2026-10-31T23",
    b"2026-11-01T00", b"2026-11-01T01", b"2026-11-01T02", b"2026-11-01T03", b"2026-11-01T04", b"2026-11-01T05",
    b"2026-11-01T06", b"2026-11-01T07", b"2026-11-01T08", b"2026-11-01T09", b"2026-11-01T10", b"2026-11-01T11",
    b"2026-11-01T12", b"2026-11-01T13", b"2026-11-01T14", b"2026-11-01T15", b"2026-11-01T16", b"2026-11-01T17",
    b"2026-11-01T18", b"2026-11-01T19", b"2026-11-01T20", b"2026-11-01T21", b"2026-11-01T22", b"2026-11-01T23",
    b"2026-11-02T00", b"2026-11-02T01", b"2026-11-02T02", b"2026-11-02T03", b"2026-11-02T04", b"2026-11-02T05",
    b"2026-11-02T06", b"2026-11-02T07", b"2026-11-02T08", b"2026-11-02T09", b"2026-11-02T10", b"2026-11-02T11",
    b"2026-11-02T12", b"2026-11-02T13", b"2026-11-02T14", b"2026-11-02T15", b"2026-11-02T16", b"2026-11-02T17",
];

const DEC_DIGITS: &[u8; 200] = b"\
0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

#[inline(always)]
fn get_hour_idx(bytes: &[u8]) -> usize {
    let h = ((bytes[11] - b'0') * 10 + (bytes[12] - b'0')) as usize;
    if bytes[6] == b'0' {
        if bytes[9] == b'0' {
            h - 18
        } else {
            6 + h
        }
    } else if bytes[9] == b'1' {
        30 + h
    } else {
        54 + h
    }
}

#[derive(Clone, Copy)]
struct Record {
    ep_offset: u32,
    ep_len: u8,
    is_5xx: u8,
    latency: u32,
}

#[inline(always)]
fn write_u64(buf: &mut Vec<u8>, mut n: u64) {
    if n == 0 {
        buf.push(b'0');
        return;
    }
    let mut tmp = [0u8; 20];
    let mut i = 20;
    while n >= 100 {
        let rem = ((n % 100) * 2) as usize;
        n /= 100;
        i -= 2;
        tmp[i] = DEC_DIGITS[rem];
        tmp[i + 1] = DEC_DIGITS[rem + 1];
    }
    if n >= 10 {
        let rem = (n * 2) as usize;
        i -= 2;
        tmp[i] = DEC_DIGITS[rem];
        tmp[i + 1] = DEC_DIGITS[rem + 1];
    } else if n > 0 {
        i -= 1;
        tmp[i] = b'0' + n as u8;
    }
    buf.extend_from_slice(&tmp[i..]);
}

#[inline(always)]
unsafe fn scan_space_avx2(data: &[u8], mut p: usize, end: usize) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        let space_vec = _mm256_set1_epi8(b' ' as i8);
        while p + 32 <= end {
            let chunk = _mm256_loadu_si256(data.as_ptr().add(p) as *const __m256i);
            let eq = _mm256_cmpeq_epi8(chunk, space_vec);
            let mask = _mm256_movemask_epi8(eq) as u32;
            if mask != 0 {
                return p + mask.trailing_zeros() as usize;
            }
            p += 32;
        }
    }
    while p < end && *data.get_unchecked(p) != b' ' {
        p += 1;
    }
    p
}

#[inline(always)]
fn fast_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut p = 0;
    while p + 8 <= bytes.len() {
        let chunk = u64::from_ne_bytes(bytes[p..p + 8].try_into().unwrap());
        hash ^= chunk;
        hash = hash.wrapping_mul(0x100000001b3);
        p += 8;
    }
    while p < bytes.len() {
        hash ^= bytes[p] as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        p += 1;
    }
    hash
}

unsafe fn libc_mmap(len: usize, fd: i32) -> *const u8 {
    unsafe extern "C" {
        fn mmap(addr: *mut core::ffi::c_void, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut core::ffi::c_void;
        fn madvise(addr: *mut core::ffi::c_void, len: usize, advice: i32) -> i32;
    }
    // MAP_POPULATE (0x8000) prefaults page table entries
    let ret = unsafe { mmap(std::ptr::null_mut(), len, 1 /* PROT_READ */, 2 /* MAP_PRIVATE */ | 0x8000 /* MAP_POPULATE */, fd, 0) };
    if ret as isize != -1 {
        unsafe {
            madvise(ret, len, 3 /* MADV_WILLNEED */);
            madvise(ret, len, 2 /* MADV_SEQUENTIAL */);
        }
    }
    ret as *const u8
}

fn main() {
    let mut data_dir = "/data".to_string();
    let mut out_dir = "/out".to_string();

    let args: Vec<String> = std::env::args().collect();
    let mut idx = 1;
    while idx < args.len() {
        if args[idx] == "--data" && idx + 1 < args.len() {
            data_dir = args[idx + 1].clone();
            idx += 2;
        } else if args[idx] == "--out" && idx + 1 < args.len() {
            out_dir = args[idx + 1].clone();
            idx += 2;
        } else if !args[idx].starts_with('-') {
            if data_dir == "/data" {
                data_dir = args[idx].clone();
            } else if out_dir == "/out" {
                out_dir = args[idx].clone();
            }
            idx += 1;
        } else {
            idx += 1;
        }
    }

    let in_path = Path::new(&data_dir).join("requests.log");
    let out_path = Path::new(&out_dir).join("summary.txt");
    let _ = std::fs::create_dir_all(&out_dir);

    let Ok(file) = File::open(&in_path) else {
        let _ = std::fs::write(&out_path, b"");
        return;
    };
    let Ok(meta) = file.metadata() else {
        let _ = std::fs::write(&out_path, b"");
        return;
    };
    let file_len = meta.len() as usize;
    if file_len == 0 {
        let _ = std::fs::write(&out_path, b"");
        return;
    }

    let ptr = unsafe { libc_mmap(file_len, file.as_raw_fd()) };
    let data = unsafe { std::slice::from_raw_parts(ptr, file_len) };

    let n_threads = if file_len < 200_000 {
        1
    } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(NUM_THREADS).min(NUM_THREADS)
    };

    let mut chunk_boundaries = Vec::with_capacity(n_threads + 1);
    chunk_boundaries.push(0);
    for i in 1..n_threads {
        let mut target = (file_len * i) / n_threads;
        while target < file_len && data[target] != b'\n' {
            target += 1;
        }
        if target < file_len {
            target += 1;
        }
        chunk_boundaries.push(target);
    }
    chunk_boundaries.push(file_len);

    // Phase 1: Worker threads bucket directly into 72 hours
    let mut handles = Vec::with_capacity(n_threads);
    for t_id in 0..n_threads {
        let start = chunk_boundaries[t_id];
        let end = chunk_boundaries[t_id + 1];
        let handle = std::thread::spawn(move || {
            let mut hour_buckets: Vec<Vec<Record>> = (0..TOTAL_HOURS)
                .map(|_| Vec::with_capacity(if n_threads == 1 { 2000 } else { 4000 }))
                .collect();
            let mut p = start;

            while p < end {
                let hour_idx = get_hour_idx(&data[p..p + 13]);
                p += 28;
                while unsafe { *data.get_unchecked(p) } != b' ' {
                    p += 1;
                }
                p += 1;

                let ep_start = p as u32;
                let ep_end = unsafe { scan_space_avx2(data, p, end) };
                let ep_len = (ep_end - p) as u8;
                p = ep_end + 1;

                let is_5xx = if unsafe { *data.get_unchecked(p) } == b'5' { 1 } else { 0 };
                p += 4;

                let mut lat: u32 = 0;
                while unsafe { *data.get_unchecked(p) } != b'\n' {
                    lat = lat * 10 + (unsafe { *data.get_unchecked(p) } - b'0') as u32;
                    p += 1;
                }
                p += 1;

                hour_buckets[hour_idx].push(Record {
                    ep_offset: ep_start,
                    ep_len,
                    is_5xx,
                    latency: lat,
                });
            }
            hour_buckets
        });
        handles.push(handle);
    }

    let mut worker_buckets: Vec<Vec<Vec<Record>>> = Vec::with_capacity(n_threads);
    for h in handles {
        worker_buckets.push(h.join().unwrap());
    }

    // Phase 2: Destination threads handle assigned hours concurrently
    let shared_workers = Arc::new(worker_buckets);
    let mut phase2_handles = Vec::with_capacity(NUM_THREADS);

    for dest_id in 0..NUM_THREADS {
        let workers = Arc::clone(&shared_workers);
        let handle = std::thread::spawn(move || {
            let base_hour = dest_id * HOURS_PER_THREAD;
            let mut out = Vec::with_capacity(1_000_000);

            for rel_h in 0..HOURS_PER_THREAD {
                let hour_idx = base_hour + rel_h;
                let hour_bytes = HOURS_STR[hour_idx];

                let mut total_records_h = 0;
                for w in 0..n_threads {
                    total_records_h += workers[w][hour_idx].len();
                }
                if total_records_h == 0 {
                    continue;
                }

                const TABLE_SIZE: usize = 4096;
                const MASK: usize = TABLE_SIZE - 1;
                let mut table = [0xFFFFu16; TABLE_SIZE];

                // In-struct 8-byte prefix eliminates L3 cache misses during endpoint sorting
                struct EpGroup {
                    prefix8: u64,
                    ep_offset: u32,
                    ep_len: u8,
                    count: u32,
                    errors: u32,
                    start_idx: u32,
                }
                let mut groups: Vec<EpGroup> = Vec::with_capacity(1850);

                // Pass 1: count records per group
                for w in 0..n_threads {
                    for r in &workers[w][hour_idx] {
                        let ep_bytes = &data[r.ep_offset as usize .. (r.ep_offset as usize + r.ep_len as usize)];
                        let hash = fast_hash(ep_bytes);
                        let mut slot = (hash as usize) & MASK;
                        loop {
                            let entry = table[slot];
                            if entry == 0xFFFF {
                                let g_id = groups.len();
                                let mut prefix_bytes = [0u8; 8];
                                let take = ep_bytes.len().min(8);
                                prefix_bytes[..take].copy_from_slice(&ep_bytes[..take]);
                                let prefix8 = u64::from_be_bytes(prefix_bytes);

                                groups.push(EpGroup {
                                    prefix8,
                                    ep_offset: r.ep_offset,
                                    ep_len: r.ep_len,
                                    count: 1,
                                    errors: r.is_5xx as u32,
                                    start_idx: 0,
                                });
                                table[slot] = g_id as u16;
                                break;
                            } else {
                                let g = &mut groups[entry as usize];
                                if g.ep_len == r.ep_len && &data[g.ep_offset as usize .. g.ep_offset as usize + g.ep_len as usize] == ep_bytes {
                                    g.count += 1;
                                    g.errors += r.is_5xx as u32;
                                    break;
                                }
                                slot = (slot + 1) & MASK;
                            }
                        }
                    }
                }

                // Prefix sums
                let mut offset = 0;
                for g in &mut groups {
                    g.start_idx = offset;
                    offset += g.count;
                }

                let mut latencies = vec![0u32; offset as usize];
                let mut cur_offsets: Vec<u32> = groups.iter().map(|g| g.start_idx).collect();

                // Pass 2: populate latencies
                for w in 0..n_threads {
                    for r in &workers[w][hour_idx] {
                        let ep_bytes = &data[r.ep_offset as usize .. (r.ep_offset as usize + r.ep_len as usize)];
                        let hash = fast_hash(ep_bytes);
                        let mut slot = (hash as usize) & MASK;
                        let g_id = loop {
                            let entry = table[slot] as usize;
                            let g = &groups[entry];
                            if g.ep_len == r.ep_len && &data[g.ep_offset as usize .. g.ep_offset as usize + g.ep_len as usize] == ep_bytes {
                                break entry;
                            }
                            slot = (slot + 1) & MASK;
                        };
                        let dest_pos = cur_offsets[g_id];
                        latencies[dest_pos as usize] = r.latency;
                        cur_offsets[g_id] += 1;
                    }
                }

                let mut sorted_group_ids: Vec<usize> = (0..groups.len()).collect();
                sorted_group_ids.sort_unstable_by(|&a, &b| {
                    let ga = &groups[a];
                    let gb = &groups[b];
                    if ga.prefix8 != gb.prefix8 {
                        ga.prefix8.cmp(&gb.prefix8)
                    } else {
                        let sa = &data[ga.ep_offset as usize .. ga.ep_offset as usize + ga.ep_len as usize];
                        let sb = &data[gb.ep_offset as usize .. gb.ep_offset as usize + gb.ep_len as usize];
                        sa.cmp(sb)
                    }
                });

                for &gid in &sorted_group_ids {
                    let g = &groups[gid];
                    let ep_bytes = &data[g.ep_offset as usize .. g.ep_offset as usize + g.ep_len as usize];
                    let lats = &mut latencies[g.start_idx as usize .. (g.start_idx + g.count) as usize];
                    let n = g.count as usize;

                    let (p50, p95, p99) = if n == 1 {
                        (lats[0], lats[0], lats[0])
                    } else {
                        let k50 = (50 * n + 99) / 100 - 1;
                        let k95 = (95 * n + 99) / 100 - 1;
                        let k99 = (99 * n + 99) / 100 - 1;
                        lats.sort_unstable();
                        (lats[k50], lats[k95], lats[k99])
                    };

                    let ppm = (g.errors as u64 * 1_000_000) / (n as u64);

                    out.extend_from_slice(hour_bytes);
                    out.extend_from_slice(b":00:00Z ");
                    out.extend_from_slice(ep_bytes);
                    out.push(b' ');
                    write_u64(&mut out, n as u64);
                    out.push(b' ');
                    write_u64(&mut out, p50 as u64);
                    out.push(b' ');
                    write_u64(&mut out, p95 as u64);
                    out.push(b' ');
                    write_u64(&mut out, p99 as u64);
                    out.push(b' ');
                    write_u64(&mut out, g.errors as u64);
                    out.push(b' ');
                    write_u64(&mut out, ppm);
                    out.push(b'\n');
                }
            }
            out
        });
        phase2_handles.push(handle);
    }

    let mut total_output = Vec::with_capacity(12_000_000);
    for h in phase2_handles {
        total_output.append(&mut h.join().unwrap());
    }

    let _ = std::fs::write(&out_path, &total_output);
}
