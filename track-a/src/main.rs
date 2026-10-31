use std::fs::File;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::Arc;

const NUM_THREADS: usize = 12;
const TOTAL_HOURS: usize = 72;
const HOURS_PER_THREAD: usize = 6;
const MAX_ENDPOINTS: usize = 2048;

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
fn get_hour_idx(b: &[u8]) -> usize {
    let h = ((b[11] - b'0') * 10 + (b[12] - b'0')) as usize;
    if b[6] == b'0' {
        if b[9] == b'0' { h - 18 } else { 6 + h }
    } else if b[9] == b'1' {
        30 + h
    } else {
        54 + h
    }
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
unsafe fn scan_space_avx2(data: *const u8, mut p: usize, end: usize) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        let space_vec = _mm256_set1_epi8(b' ' as i8);
        while p + 32 <= end {
            let chunk = _mm256_loadu_si256(data.add(p) as *const __m256i);
            let eq = _mm256_cmpeq_epi8(chunk, space_vec);
            let mask = _mm256_movemask_epi8(eq) as u32;
            if mask != 0 {
                return p + mask.trailing_zeros() as usize;
            }
            p += 32;
        }
    }
    while p < end && *data.add(p) != b' ' {
        p += 1;
    }
    p
}

#[inline(always)]
unsafe fn fast_hash(ptr: *const u8, len: usize) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut p = 0;
    while p + 8 <= len {
        let chunk = (ptr.add(p) as *const u64).read_unaligned();
        hash ^= chunk;
        hash = hash.wrapping_mul(0x100000001b3);
        p += 8;
    }
    while p < len {
        hash ^= *ptr.add(p) as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        p += 1;
    }
    hash
}

#[inline(always)]
unsafe fn ep_eq(ptr: *const u8, o1: usize, o2: usize, len: usize) -> bool {
    let mut i = 0;
    while i + 8 <= len {
        if (ptr.add(o1 + i) as *const u64).read_unaligned() != (ptr.add(o2 + i) as *const u64).read_unaligned() {
            return false;
        }
        i += 8;
    }
    while i < len {
        if *ptr.add(o1 + i) != *ptr.add(o2 + i) {
            return false;
        }
        i += 1;
    }
    true
}

unsafe fn libc_mmap(len: usize, fd: i32) -> *const u8 {
    unsafe extern "C" {
        fn mmap(addr: *mut core::ffi::c_void, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut core::ffi::c_void;
        fn madvise(addr: *mut core::ffi::c_void, len: usize, advice: i32) -> i32;
    }
    let ret = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, fd, 0) };
    if ret as isize != -1 {
        unsafe { madvise(ret, len, 2); }
    }
    ret as *const u8
}

#[derive(Clone, Copy)]
struct EpSlice {
    offset: u32,
    len: u16,
}

struct HourData {
    packed_eps: Vec<u16>,
    lats: Vec<u32>,
}

struct WorkerResult {
    local_eps: Vec<EpSlice>,
    hours: Vec<HourData>,
}

fn main() {
    let (data_dir, out_dir) = {
        let args: Vec<String> = std::env::args().collect();
        let mut d = "/data".to_string();
        let mut o = "/out".to_string();
        let mut i = 1;
        while i < args.len() {
            if args[i] == "--data" && i + 1 < args.len() { d = args[i + 1].clone(); i += 2; }
            else if args[i] == "--out" && i + 1 < args.len() { o = args[i + 1].clone(); i += 2; }
            else if !args[i].starts_with('-') {
                if d == "/data" { d = args[i].clone(); } else if o == "/out" { o = args[i].clone(); }
                i += 1;
            } else { i += 1; }
        }
        (d, o)
    };

    let in_path = Path::new(&data_dir).join("requests.log");
    let out_path = Path::new(&out_dir).join("summary.txt");
    let _ = std::fs::create_dir_all(&out_dir);

    let Ok(file) = File::open(&in_path) else { let _ = std::fs::write(&out_path, b""); return; };
    let Ok(meta) = file.metadata() else { let _ = std::fs::write(&out_path, b""); return; };
    let file_len = meta.len() as usize;
    if file_len == 0 { let _ = std::fs::write(&out_path, b""); return; }

    let ptr = unsafe { libc_mmap(file_len, file.as_raw_fd()) };
    let data = unsafe { std::slice::from_raw_parts(ptr, file_len) };

    let n_threads = if file_len < 200_000 { 1 } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(NUM_THREADS).min(NUM_THREADS)
    };

    let mut chunk_boundaries = Vec::with_capacity(n_threads + 1);
    chunk_boundaries.push(0);
    for i in 1..n_threads {
        let mut target = (file_len * i) / n_threads;
        while target < file_len && data[target] != b'\n' { target += 1; }
        if target < file_len { target += 1; }
        chunk_boundaries.push(target);
    }
    chunk_boundaries.push(file_len);

    let mut handles = Vec::with_capacity(n_threads);
    for t_id in 0..n_threads {
        let start = chunk_boundaries[t_id];
        let end = chunk_boundaries[t_id + 1];
        handles.push(std::thread::spawn(move || {
            let cap = if n_threads == 1 { 45000 } else { 4200 };
            let mut hours: Vec<HourData> = (0..TOTAL_HOURS)
                .map(|_| HourData { packed_eps: Vec::with_capacity(cap), lats: Vec::with_capacity(cap) })
                .collect();
            let mut table = [0xFFFFu16; 4096];
            let mut local_eps: Vec<EpSlice> = Vec::with_capacity(MAX_ENDPOINTS);

            let mut p = start;
            while p < end {
                let hour_idx = get_hour_idx(&data[p..p + 13]);
                let ep_start = match unsafe { *data.get_unchecked(p + 28) } {
                    b'G' => p + 32,
                    b'P' => match unsafe { *data.get_unchecked(p + 29) } {
                        b'O' => p + 33,
                        b'U' => p + 32,
                        _ => p + 34,
                    },
                    b'D' => p + 35,
                    b'H' => p + 33,
                    _ => p + 36,
                };

                let ep_end = unsafe { scan_space_avx2(data.as_ptr(), ep_start, end) };
                let ep_len = (ep_end - ep_start) as u16;
                let is_5xx: u16 = if unsafe { *data.get_unchecked(ep_end + 1) } == b'5' { 1 } else { 0 };

                let mut lat_p = ep_end + 5;
                let mut lat: u32 = 0;
                while unsafe { *data.get_unchecked(lat_p) } != b'\n' {
                    lat = lat * 10 + (unsafe { *data.get_unchecked(lat_p) } - b'0') as u32;
                    lat_p += 1;
                }
                p = lat_p + 1;

                let hash = unsafe { fast_hash(data.as_ptr().add(ep_start), ep_len as usize) };
                let mut slot = (hash as usize) & 4095;
                let local_id = loop {
                    let entry = table[slot];
                    if entry == 0xFFFF {
                        let id = local_eps.len() as u16;
                        local_eps.push(EpSlice { offset: ep_start as u32, len: ep_len });
                        table[slot] = id;
                        break id;
                    } else {
                        let ep = &local_eps[entry as usize];
                        if ep.len == ep_len && unsafe { ep_eq(data.as_ptr(), ep.offset as usize, ep_start, ep_len as usize) } {
                            break entry;
                        }
                        slot = (slot + 1) & 4095;
                    }
                };

                hours[hour_idx].packed_eps.push(local_id | (is_5xx << 15));
                hours[hour_idx].lats.push(lat);
            }
            WorkerResult { local_eps, hours }
        }));
    }

    let mut worker_results: Vec<WorkerResult> = Vec::with_capacity(n_threads);
    for h in handles { worker_results.push(h.join().unwrap()); }

    let mut canonical_eps: Vec<EpSlice> = Vec::with_capacity(MAX_ENDPOINTS);
    let mut dedup_table = [0xFFFFu16; 4096];
    for w in 0..n_threads {
        for ep in &worker_results[w].local_eps {
            let hash = unsafe { fast_hash(data.as_ptr().add(ep.offset as usize), ep.len as usize) };
            let mut slot = (hash as usize) & 4095;
            loop {
                let entry = dedup_table[slot];
                if entry == 0xFFFF {
                    let id = canonical_eps.len() as u16;
                    canonical_eps.push(*ep);
                    dedup_table[slot] = id;
                    break;
                } else {
                    let ex = &canonical_eps[entry as usize];
                    if ex.len == ep.len && unsafe { ep_eq(data.as_ptr(), ex.offset as usize, ep.offset as usize, ep.len as usize) } {
                        break;
                    }
                    slot = (slot + 1) & 4095;
                }
            }
        }
    }

    canonical_eps.sort_unstable_by(|a, b| {
        let sa = unsafe { data.get_unchecked(a.offset as usize..a.offset as usize + a.len as usize) };
        let sb = unsafe { data.get_unchecked(b.offset as usize..b.offset as usize + b.len as usize) };
        sa.cmp(sb)
    });

    let n_canon = canonical_eps.len();
    let mut local_to_canonical: Vec<Vec<u16>> = Vec::with_capacity(n_threads);
    for w in 0..n_threads {
        let mut mapping = Vec::with_capacity(worker_results[w].local_eps.len());
        for ep in &worker_results[w].local_eps {
            let ep_bytes = unsafe { data.get_unchecked(ep.offset as usize..ep.offset as usize + ep.len as usize) };
            let idx = canonical_eps.binary_search_by(|probe| {
                let probe_bytes = unsafe { data.get_unchecked(probe.offset as usize..probe.offset as usize + probe.len as usize) };
                probe_bytes.cmp(ep_bytes)
            }).unwrap();
            mapping.push(idx as u16);
        }
        local_to_canonical.push(mapping);
    }

    let shared_results = Arc::new(worker_results);
    let shared_mappings = Arc::new(local_to_canonical);
    let shared_canon = Arc::new(canonical_eps);

    let mut phase2_handles = Vec::with_capacity(NUM_THREADS);
    for dest_id in 0..NUM_THREADS {
        let results = Arc::clone(&shared_results);
        let mappings = Arc::clone(&shared_mappings);
        let canon = Arc::clone(&shared_canon);

        phase2_handles.push(std::thread::spawn(move || {
            let base_hour = dest_id * HOURS_PER_THREAD;
            let mut out = Vec::with_capacity(600_000);
            let mut counts = vec![0u32; n_canon];
            let mut errors = vec![0u32; n_canon];
            let mut offsets = vec![0u32; n_canon];
            let mut cur_offsets = vec![0u32; n_canon];
            let mut sorted_lats: Vec<u32> = Vec::with_capacity(50_000);

            for rel_h in 0..HOURS_PER_THREAD {
                let hour_idx = base_hour + rel_h;
                let hour_bytes = HOURS_STR[hour_idx];

                counts.fill(0);
                errors.fill(0);

                for w in 0..n_threads {
                    let map = &mappings[w];
                    for &packed in &results[w].hours[hour_idx].packed_eps {
                        let canon_id = map[(packed & 0x7FFF) as usize] as usize;
                        counts[canon_id] += 1;
                        errors[canon_id] += (packed >> 15) as u32;
                    }
                }

                let mut total: u32 = 0;
                for i in 0..n_canon {
                    offsets[i] = total;
                    cur_offsets[i] = total;
                    total += counts[i];
                }
                if total == 0 { continue; }
                if sorted_lats.len() < total as usize { sorted_lats.resize(total as usize, 0); }

                for w in 0..n_threads {
                    let map = &mappings[w];
                    let eps = &results[w].hours[hour_idx].packed_eps;
                    let lats = &results[w].hours[hour_idx].lats;
                    for i in 0..eps.len() {
                        let canon_id = map[(eps[i] & 0x7FFF) as usize] as usize;
                        let dest = cur_offsets[canon_id];
                        cur_offsets[canon_id] += 1;
                        sorted_lats[dest as usize] = lats[i];
                    }
                }

                for canon_id in 0..n_canon {
                    let n = counts[canon_id] as usize;
                    if n == 0 { continue; }
                    let start = offsets[canon_id] as usize;
                    let slice = &mut sorted_lats[start..start + n];

                    let (p50, p95, p99) = if n == 1 {
                        (slice[0], slice[0], slice[0])
                    } else {
                        slice.sort_unstable();
                        (
                            slice[(50 * n + 99) / 100 - 1],
                            slice[(95 * n + 99) / 100 - 1],
                            slice[(99 * n + 99) / 100 - 1],
                        )
                    };

                    let err = errors[canon_id];
                    let ppm = (err as u64 * 1_000_000) / (n as u64);
                    let ep = &canon[canon_id];
                    let ep_bytes = unsafe { data.get_unchecked(ep.offset as usize..ep.offset as usize + ep.len as usize) };

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
                    write_u64(&mut out, err as u64);
                    out.push(b' ');
                    write_u64(&mut out, ppm);
                    out.push(b'\n');
                }
            }
            out
        }));
    }

    let mut file = File::create(&out_path).expect("create output");
    for h in phase2_handles {
        let buf = h.join().unwrap();
        file.write_all(&buf).expect("write output");
    }
}
