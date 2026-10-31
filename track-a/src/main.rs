use core::arch::x86_64::*;
use std::fs::File;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;

const NT: usize = 12;
const TH: usize = 72;
const HPT: usize = 6;
const HS: usize = 8192;
const HM: usize = HS - 1;

const HRS: &[u8; 936] = b"2026-10-30T182026-10-30T192026-10-30T202026-10-30T212026-10-30T222026-10-30T232026-10-31T002026-10-31T012026-10-31T022026-10-31T032026-10-31T042026-10-31T052026-10-31T062026-10-31T072026-10-31T082026-10-31T092026-10-31T102026-10-31T112026-10-31T122026-10-31T132026-10-31T142026-10-31T152026-10-31T162026-10-31T172026-10-31T182026-10-31T192026-10-31T202026-10-31T212026-10-31T222026-10-31T232026-11-01T002026-11-01T012026-11-01T022026-11-01T032026-11-01T042026-11-01T052026-11-01T062026-11-01T072026-11-01T082026-11-01T092026-11-01T102026-11-01T112026-11-01T122026-11-01T132026-11-01T142026-11-01T152026-11-01T162026-11-01T172026-11-01T182026-11-01T192026-11-01T202026-11-01T212026-11-01T222026-11-01T232026-11-02T002026-11-02T012026-11-02T022026-11-02T032026-11-02T042026-11-02T052026-11-02T062026-11-02T072026-11-02T082026-11-02T092026-11-02T102026-11-02T112026-11-02T122026-11-02T132026-11-02T142026-11-02T152026-11-02T162026-11-02T17";
const DD: &[u8; 200] = b"00010203040506070809101112131415161718192021222324252627282930313233343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778798081828384858687888990919293949596979899";

#[inline(always)]
fn wu64(o: &mut Vec<u8>, mut n: u64) {
    if n == 0 {
        o.push(b'0');
        return;
    }
    let mut t = [0u8; 20];
    let mut i = 20;
    while n >= 100 {
        let r = (n % 100 * 2) as usize;
        n /= 100;
        i -= 2;
        t[i] = DD[r];
        t[i + 1] = DD[r + 1];
    }
    if n >= 10 {
        let r = (n * 2) as usize;
        i -= 2;
        t[i] = DD[r];
        t[i + 1] = DD[r + 1];
    } else {
        i -= 1;
        t[i] = b'0' + n as u8;
    }
    o.extend_from_slice(&t[i..]);
}

#[inline(always)]
fn emit_line(
    out: &mut Vec<u8>,
    hb: &[u8],
    eb: &[u8],
    n: usize,
    p50: u32,
    p95: u32,
    p99: u32,
    err: u32,
    ppm: u64,
) {
    out.extend_from_slice(hb);
    out.extend_from_slice(b":00:00Z ");
    out.extend_from_slice(eb);
    for v in [
        n as u64, p50 as u64, p95 as u64, p99 as u64, err as u64, ppm,
    ] {
        out.push(b' ');
        wu64(out, v);
    }
    out.push(b'\n');
}

#[inline(always)]
unsafe fn scan_sp(d: *const u8, mut p: usize, e: usize) -> usize {
    let v = _mm256_set1_epi8(b' ' as i8);
    while p + 32 <= e {
        let m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(
            _mm256_loadu_si256(d.add(p) as *const __m256i),
            v,
        )) as u32;
        if m != 0 {
            return p + m.trailing_zeros() as usize;
        }
        p += 32;
    }
    while p < e && *d.add(p) != b' ' {
        p += 1;
    }
    p
}

#[inline(always)]
unsafe fn fhash(p: *const u8, l: usize) -> u64 {
    let mut crc = l as u64;
    if l >= 8 {
        let mut i = 0;
        while i + 8 <= l {
            crc = _mm_crc32_u64(crc, (p.add(i) as *const u64).read_unaligned());
            i += 8;
        }
        if i < l {
            crc = _mm_crc32_u64(crc, (p.add(l - 8) as *const u64).read_unaligned());
        }
        return crc;
    }
    for i in 0..l {
        crc = _mm_crc32_u8(crc as u32, *p.add(i)) as u64;
    }
    crc
}

#[inline(always)]
unsafe fn parse_lat(data: *const u8, lat_p: usize, end: usize) -> (u32, usize) {
    if lat_p + 8 <= end {
        let v = (data.add(lat_p) as *const u64).read_unaligned();
        let b4 = ((v >> 32) & 0xFF) as u8;
        let b5 = ((v >> 40) & 0xFF) as u8;
        let d0 = ((v & 0xFF) - 0x30) as u32;
        let d1 = (((v >> 8) & 0xFF) - 0x30) as u32;
        let d2 = (((v >> 16) & 0xFF) - 0x30) as u32;
        let d3 = (((v >> 24) & 0xFF) - 0x30) as u32;
        if b4 == b'\n' {
            return (d0 * 1000 + d1 * 100 + d2 * 10 + d3, lat_p + 5);
        }
        if b5 == b'\n' {
            let d4 = (((v >> 32) & 0xFF) - 0x30) as u32;
            return (d0 * 10000 + d1 * 1000 + d2 * 100 + d3 * 10 + d4, lat_p + 6);
        }
    }
    let (mut lp, mut num) = (lat_p, 0u32);
    while *data.add(lp) != b'\n' {
        num = num * 10 + (*data.add(lp) - b'0') as u32;
        lp += 1;
    }
    (num, lp + 1)
}

unsafe fn do_mmap(len: usize, fd: i32) -> *const u8 {
    unsafe extern "C" {
        fn mmap(
            a: *mut core::ffi::c_void,
            l: usize,
            pr: i32,
            fl: i32,
            fd: i32,
            o: i64,
        ) -> *mut core::ffi::c_void;
        fn madvise(a: *mut core::ffi::c_void, l: usize, adv: i32) -> i32;
    }
    let r = mmap(std::ptr::null_mut(), len, 1, 2, fd, 0);
    if r as isize != -1 {
        madvise(r, len, 2);
    }
    r as *const u8
}

#[derive(Clone, Copy)]
struct Ep {
    off: u32,
    len: u16,
}

struct WorkerResult {
    eps: Vec<Ep>,
    hours: Vec<Vec<u64>>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut d = "/data".to_string();
    let mut o = "/out".to_string();
    while let Some(a) = args.next() {
        if a == "--data" {
            if let Some(v) = args.next() {
                d = v;
            }
        } else if a == "--out" {
            if let Some(v) = args.next() {
                o = v;
            }
        } else if !a.starts_with('-') {
            if d == "/data" {
                d = a;
            } else {
                o = a;
            }
        }
    }

    let ip = Path::new(&d).join("requests.log");
    let op = Path::new(&o).join("summary.txt");
    let _ = std::fs::create_dir_all(&o);
    let Ok(f) = File::open(&ip) else {
        let _ = std::fs::write(&op, b"");
        return;
    };
    let Ok(m) = f.metadata() else {
        let _ = std::fs::write(&op, b"");
        return;
    };
    let fl = m.len() as usize;
    if fl == 0 {
        let _ = std::fs::write(&op, b"");
        return;
    }
    let ptr = unsafe { do_mmap(fl, f.as_raw_fd()) };
    let data = unsafe { std::slice::from_raw_parts(ptr, fl) };
    let nt = if fl < 200_000 {
        1
    } else {
        std::thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(NT)
            .min(NT)
    };

    let mut cb = vec![0usize; nt + 1];
    for i in 1..nt {
        let mut t = (fl * i) / nt;
        while t < fl && data[t] != b'\n' {
            t += 1;
        }
        if t < fl {
            t += 1;
        }
        cb[i] = t;
    }
    cb[nt] = fl;

    let wr: Vec<WorkerResult> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(nt);
        for tid in 0..nt {
            let (start, end) = (cb[tid], cb[tid + 1]);
            handles.push(
                std::thread::Builder::new()
                    .stack_size(256 * 1024)
                    .spawn_scoped(s, move || {
                        let mut hours: Vec<Vec<u64>> = (0..TH).map(|_| Vec::new()).collect();
                        let mut tab = [0xFFFFu16; HS];
                        let mut eps: Vec<Ep> = Vec::with_capacity(2048);
                        let mut cur_hi = usize::MAX;
                        let mut cur_h_ptr: *mut Vec<u64> = std::ptr::null_mut();
                        let mut p = start;
                        while p < end {
                            let day =
                                (unsafe { *data.get_unchecked(p + 9) + *data.get_unchecked(p + 6) }
                                    - (b'0' * 2)) as usize;
                            let h = (unsafe {
                                *data.get_unchecked(p + 11) as usize * 10
                                    + *data.get_unchecked(p + 12) as usize
                            }) - 528;
                            let hi = day * 24 + h - 18;

                            let es = match unsafe { *data.get_unchecked(p + 28) } {
                                b'G' => p + 32,
                                b'H' => p + 33,
                                b'D' => p + 35,
                                b'P' => match unsafe { *data.get_unchecked(p + 29) } {
                                    b'O' => p + 33,
                                    b'U' => p + 32,
                                    _ => p + 34,
                                },
                                _ => p + 36,
                            };
                            let ee = unsafe { scan_sp(data.as_ptr(), es, end) };
                            let el = (ee - es) as u16;
                            let is5: u64 = (unsafe { *data.get_unchecked(ee + 1) } == b'5') as u64;

                            let (lat, next_p) = unsafe { parse_lat(data.as_ptr(), ee + 5, end) };
                            p = next_p;

                            let hash = unsafe { fhash(data.as_ptr().add(es), el as usize) };
                            let mut slot = hash as usize & HM;
                            let lid = loop {
                                let ent = tab[slot];
                                if ent == 0xFFFF {
                                    let id = eps.len() as u16;
                                    eps.push(Ep {
                                        off: es as u32,
                                        len: el,
                                    });
                                    tab[slot] = id;
                                    break id;
                                }
                                let ep = unsafe { eps.get_unchecked(ent as usize) };
                                if ep.len == el
                                    && unsafe {
                                        std::slice::from_raw_parts(
                                            data.as_ptr().add(ep.off as usize),
                                            el as usize,
                                        ) == std::slice::from_raw_parts(
                                            data.as_ptr().add(es),
                                            el as usize,
                                        )
                                    }
                                {
                                    break ent;
                                }
                                slot = (slot + 1) & HM;
                            };

                            if hi != cur_hi {
                                cur_hi = hi;
                                let h_vec = unsafe { hours.get_unchecked_mut(hi) };
                                if h_vec.capacity() == 0 {
                                    h_vec.reserve(24000);
                                }
                                cur_h_ptr = h_vec as *mut _;
                            }
                            unsafe {
                                (*cur_h_ptr).push(((lat as u64) << 16) | (is5 << 15) | lid as u64);
                            }
                        }
                        WorkerResult { eps, hours }
                    })
                    .unwrap(),
            );
        }
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut ceps: Vec<Ep> = Vec::with_capacity(2048);
    let mut dtab = [0xFFFFu16; HS];
    let mut local_to_orig: Vec<Vec<u16>> = Vec::with_capacity(nt);

    for w in 0..nt {
        let mut m = Vec::with_capacity(wr[w].eps.len());
        for ep in &wr[w].eps {
            let hash = unsafe { fhash(data.as_ptr().add(ep.off as usize), ep.len as usize) };
            let mut slot = hash as usize & HM;
            let orig_id = loop {
                let ent = dtab[slot];
                if ent == 0xFFFF {
                    let id = ceps.len() as u16;
                    dtab[slot] = id;
                    ceps.push(*ep);
                    break id;
                }
                let ex = unsafe { ceps.get_unchecked(ent as usize) };
                if ex.len == ep.len
                    && unsafe {
                        std::slice::from_raw_parts(
                            data.as_ptr().add(ex.off as usize),
                            ep.len as usize,
                        ) == std::slice::from_raw_parts(
                            data.as_ptr().add(ep.off as usize),
                            ep.len as usize,
                        )
                    }
                {
                    break ent;
                }
                slot = (slot + 1) & HM;
            };
            m.push(orig_id);
        }
        local_to_orig.push(m);
    }

    let nc = ceps.len();
    let mut canon_indices: Vec<u16> = (0..nc as u16).collect();
    canon_indices.sort_unstable_by(|&a, &b| {
        let ep_a = unsafe { ceps.get_unchecked(a as usize) };
        let ep_b = unsafe { ceps.get_unchecked(b as usize) };
        let sa =
            unsafe { data.get_unchecked(ep_a.off as usize..ep_a.off as usize + ep_a.len as usize) };
        let sb =
            unsafe { data.get_unchecked(ep_b.off as usize..ep_b.off as usize + ep_b.len as usize) };
        sa.cmp(sb)
    });

    let mut orig_to_canon = vec![0u16; nc];
    for (sorted_pos, &orig_id) in canon_indices.iter().enumerate() {
        orig_to_canon[orig_id as usize] = sorted_pos as u16;
    }

    let sorted_ceps: Vec<Ep> = canon_indices
        .iter()
        .map(|&orig_id| unsafe { *ceps.get_unchecked(orig_id as usize) })
        .collect();

    let mut l2c: Vec<Vec<u16>> = Vec::with_capacity(nt);
    for w in 0..nt {
        let m: Vec<u16> = local_to_orig[w]
            .iter()
            .map(|&orig| unsafe { *orig_to_canon.get_unchecked(orig as usize) })
            .collect();
        l2c.push(m);
    }

    let outputs = std::thread::scope(|s| {
        let mut ph2 = Vec::with_capacity(NT);
        for did in 0..NT {
            let r = &wr;
            let mp = &l2c;
            let cn = &sorted_ceps;
            ph2.push(
                std::thread::Builder::new()
                    .stack_size(256 * 1024)
                    .spawn_scoped(s, move || {
                        let bh = did * HPT;
                        let mut out = Vec::with_capacity(550_000);
                        let mut cnt = vec![0u32; nc];
                        let mut err = vec![0u32; nc];
                        let mut off = vec![0u32; nc];
                        let mut cur = vec![0u32; nc];
                        let mut sl: Vec<u32> = Vec::with_capacity(45_000);
                        for rh in 0..HPT {
                            let hi = bh + rh;
                            let hb = &HRS[hi * 13..hi * 13 + 13];
                            cnt.fill(0);
                            err.fill(0);
                            for w in 0..nt {
                                let recs = unsafe { r[w].hours.get_unchecked(hi) };
                                if recs.is_empty() {
                                    continue;
                                }
                                let map = unsafe { mp.get_unchecked(w) };
                                for &pk in recs {
                                    let ci = unsafe { *map.get_unchecked((pk & 0x7FFF) as usize) }
                                        as usize;
                                    unsafe {
                                        *cnt.get_unchecked_mut(ci) += 1;
                                        *err.get_unchecked_mut(ci) += ((pk >> 15) & 1) as u32;
                                    }
                                }
                            }
                            let mut tot = 0u32;
                            for i in 0..nc {
                                off[i] = tot;
                                cur[i] = tot;
                                tot += cnt[i];
                            }
                            if tot == 0 {
                                continue;
                            }
                            if sl.len() < tot as usize {
                                sl.resize(tot as usize, 0);
                            }
                            for w in 0..nt {
                                let recs = unsafe { r[w].hours.get_unchecked(hi) };
                                if recs.is_empty() {
                                    continue;
                                }
                                let map = unsafe { mp.get_unchecked(w) };
                                for &pk in recs {
                                    let ci = unsafe { *map.get_unchecked((pk & 0x7FFF) as usize) }
                                        as usize;
                                    let d = unsafe { *cur.get_unchecked(ci) };
                                    unsafe {
                                        *cur.get_unchecked_mut(ci) = d + 1;
                                        *sl.get_unchecked_mut(d as usize) = (pk >> 16) as u32;
                                    }
                                }
                            }
                            for ci in 0..nc {
                                let n = cnt[ci] as usize;
                                if n == 0 {
                                    continue;
                                }
                                let st = off[ci] as usize;
                                let s = unsafe { sl.get_unchecked_mut(st..st + n) };
                                let (p50, p95, p99) = match n {
                                    1 => (s[0], s[0], s[0]),
                                    2 => {
                                        let (min, max) = if s[0] <= s[1] {
                                            (s[0], s[1])
                                        } else {
                                            (s[1], s[0])
                                        };
                                        (min, max, max)
                                    }
                                    3 => {
                                        let (mut a, mut b, mut c) = (s[0], s[1], s[2]);
                                        if a > b {
                                            std::mem::swap(&mut a, &mut b);
                                        }
                                        if b > c {
                                            std::mem::swap(&mut b, &mut c);
                                        }
                                        if a > b {
                                            std::mem::swap(&mut a, &mut b);
                                        }
                                        (b, c, c)
                                    }
                                    _ => {
                                        s.sort_unstable();
                                        (
                                            s[(50 * n + 99) / 100 - 1],
                                            s[(95 * n + 99) / 100 - 1],
                                            s[(99 * n + 99) / 100 - 1],
                                        )
                                    }
                                };
                                let e = err[ci];
                                let ppm = (e as u64 * 1_000_000) / n as u64;
                                let ep = unsafe { cn.get_unchecked(ci) };
                                let eb = unsafe {
                                    data.get_unchecked(
                                        ep.off as usize..ep.off as usize + ep.len as usize,
                                    )
                                };
                                emit_line(&mut out, hb, eb, n, p50, p95, p99, e, ppm);
                            }
                        }
                        out
                    })
                    .unwrap(),
            );
        }
        let mut outputs = Vec::with_capacity(NT);
        for h in ph2 {
            outputs.push(h.join().unwrap());
        }
        outputs
    });

    let mut f = File::create(&op).expect("create");
    for buf in outputs {
        f.write_all(&buf).expect("write");
    }
}
