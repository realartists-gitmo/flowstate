//! Faithful port of mimalloc `test/test-stress.c`: multiple threads
//! allocating linearly-distributed power-of-two sizes, transferring blocks
//! between threads, with threads destroyed and recreated each iteration.
//!
//! Usage: stress [THREADS] [SCALE] [ITER]

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

const TRANSFERS: usize = 1000;
const COOKIE: usize = 0xbf58476d1ce4e5b9;

static TRANSFER: [AtomicPtr<usize>; TRANSFERS] =
    [const { AtomicPtr::new(std::ptr::null_mut()) }; TRANSFERS];

static ALLOW_LARGE_OBJECTS: bool = false;

/// splitmix64 (matching the C test)
fn pick(r: &mut usize) -> usize {
    let mut x = *r;
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58476d1ce4e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    *r = x;
    x
}

fn chance(perc: usize, r: &mut usize) -> bool {
    pick(r) % 100 <= perc
}

fn alloc_items(mut items: usize, r: &mut usize) -> *mut usize {
    if chance(1, r) {
        if chance(1, r) && ALLOW_LARGE_OBJECTS {
            items *= 10000; // 0.01% giant
        } else if chance(10, r) && ALLOW_LARGE_OBJECTS {
            items *= 1000; // 0.1% huge
        } else {
            items *= 100; // 1% large objects
        }
    }
    if (32..=40).contains(&items) {
        items *= 2; // pthreads uses 320b allocations
    }
    if items == 0 {
        items = 1;
    }
    let p = rimalloc::calloc(items, size_of::<usize>()).cast::<usize>();
    if !p.is_null() {
        for i in 0..items {
            unsafe {
                // C builds the reference harness with NDEBUG, compiling out
                // its `assert(p[i] == 0)`; keep instruction parity here.
                debug_assert_eq!(*p.add(i), 0);
                *p.add(i) = (items - i) ^ COOKIE;
            }
        }
    }
    p
}

fn free_items(p: *mut usize) {
    if !p.is_null() {
        unsafe {
            let items = *p ^ COOKIE;
            for i in 0..items {
                assert_eq!(
                    *p.add(i) ^ COOKIE,
                    items - i,
                    "memory corruption at block {p:p} at {i}"
                );
            }
        }
    }
    rimalloc::free(p.cast());
}

fn stress(tid: usize, scale: usize) {
    let mut r: usize = (tid + 1) * 43;
    const MAX_ITEM_SHIFT: usize = 5; // 128
    const MAX_ITEM_RETAINED_SHIFT: usize = MAX_ITEM_SHIFT + 2;
    let mut allocs = 100 * scale * (tid % 8 + 1); // some threads do more
    let mut retain = allocs / 2;
    let mut data: Vec<*mut usize> = Vec::new();
    let mut retained: Vec<*mut usize> = Vec::with_capacity(retain);

    while allocs > 0 || retain > 0 {
        if retain == 0 || (chance(50, &mut r) && allocs > 0) {
            // 50%+ alloc
            allocs -= 1;
            data.push(alloc_items(1 << (pick(&mut r) % MAX_ITEM_SHIFT), &mut r));
        } else {
            // 25% retain
            retained.push(alloc_items(
                1 << (pick(&mut r) % MAX_ITEM_RETAINED_SHIFT),
                &mut r,
            ));
            retain -= 1;
        }
        if chance(66, &mut r) && !data.is_empty() {
            // 66% free previous alloc
            let idx = pick(&mut r) % data.len();
            free_items(data[idx]);
            data[idx] = std::ptr::null_mut();
        }
        if chance(25, &mut r) && !data.is_empty() {
            // 25% exchange a local pointer with the (shared) transfer buffer
            let data_idx = pick(&mut r) % data.len();
            let transfer_idx = pick(&mut r) % TRANSFERS;
            let p = data[data_idx];
            let q = TRANSFER[transfer_idx].swap(p, Ordering::AcqRel);
            data[data_idx] = q;
        }
    }
    // free everything that is left
    for p in retained {
        free_items(p);
    }
    for p in data {
        free_items(p);
    }
}

static SCALE: AtomicUsize = AtomicUsize::new(0);

fn run_os_threads(nthreads: usize, scale: usize) {
    SCALE.store(scale, Ordering::Relaxed);
    let scale = SCALE.load(Ordering::Relaxed);
    std::thread::scope(|s| {
        for tid in 0..nthreads {
            s.spawn(move || {
                stress(tid, scale);
            });
        }
    });
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let threads: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(32);
    let scale: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50);
    let iter: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(50);
    println!("stress: {threads} threads, scale {scale}, {iter} iterations");

    let start = std::time::Instant::now();
    let mut r: usize = 42;
    for n in 0..iter {
        run_os_threads(threads, scale);
        for t in &TRANSFER {
            if chance(50, &mut r) || n + 1 == iter {
                // free all on last run, otherwise free half
                let p = t.swap(std::ptr::null_mut(), Ordering::AcqRel);
                free_items(p);
            }
        }
        if (n + 1) % 10 == 0 {
            println!("- iterations left: {:3}", iter - (n + 1));
        }
    }
    rimalloc::collect(true);
    println!(
        "stress test completed successfully in {:.2?}",
        start.elapsed()
    );
    let [mmap, munmap, madvise, mprotect] = rimalloc::os_syscall_counts();
    println!("syscalls: mmap={mmap} munmap={munmap} madvise={madvise} mprotect={mprotect}");
    let (abandoned, tries, hits) = rimalloc::abandoned_stats();
    println!("abandoned={abandoned} reclaim_tries={tries} reclaim_hits={hits}");
    if std::env::var_os("STRESS_STATS").is_some() {
        rimalloc::stats_print();
    }
}
