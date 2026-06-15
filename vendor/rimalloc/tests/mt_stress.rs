//! Deterministic multithreaded stress: the same op-program interpreter as
//! the `mt_ops` fuzz target, driven by fixed PRNG-generated programs so the
//! cross-thread paths (`free_generic_mt`, thread-exit abandonment,
//! reclaim-on-free) run under `cargo test` — and, critically, under the
//! TSan battery (`scripts/tsan.sh`), where loom's extracted-protocol model
//! cannot see segment reclaim, arena bitmaps, or page lifecycle.

#![cfg(not(miri))] // covered by loom/fuzz; threads under Miri are too slow

use std::sync::mpsc;

struct Block(*mut u8, usize, u8);
unsafe impl Send for Block {}

fn fill(p: *mut u8, n: usize, pat: u8) {
    unsafe { core::ptr::write_bytes(p, pat, n.min(4096)) }
}

fn check(b: &Block) {
    for i in 0..b.1.min(4096) {
        assert_eq!(
            unsafe { *b.0.add(i) },
            b.2,
            "corruption in cross-thread block"
        );
    }
}

/// The `mt_ops` fuzz-target body, verbatim.
fn run_program(data: &[u8]) {
    if data.len() < 4 {
        return;
    }
    let n_remote = (data[0] % 3 + 1) as usize;
    let ops = &data[1..];

    let mut to_remote: Vec<mpsc::Sender<Block>> = Vec::new();
    let mut handles = Vec::new();
    let (back_tx, back_rx) = mpsc::channel::<Block>();
    for t in 0..n_remote {
        let (tx, rx) = mpsc::channel::<Block>();
        to_remote.push(tx);
        let back = back_tx.clone();
        let seed = data.get(2 + t).copied().unwrap_or(7);
        handles.push(std::thread::spawn(move || {
            let mut k = 0u8;
            while let Ok(b) = rx.recv() {
                check(&b);
                rimalloc::free(b.0);
                let size = ((seed as usize * 131 + k as usize * 977) % 9000) + 1;
                let p = rimalloc::malloc(size);
                if !p.is_null() {
                    let pat = seed ^ k | 1;
                    fill(p, size, pat);
                    if k % 3 == 0 {
                        let _ = back.send(Block(p, size, pat));
                    } else {
                        check(&Block(p, size, pat));
                        rimalloc::free(p);
                    }
                }
                k = k.wrapping_add(1);
            }
        }));
    }
    drop(back_tx);

    let mut local: Vec<Block> = Vec::new();
    let mut pat = 0x10u8;
    for (i, &op) in ops.iter().enumerate() {
        match op % 5 {
            0 | 1 => {
                let size = ((op as usize) << 5 | i & 31) + 1;
                let p = rimalloc::malloc(size);
                if !p.is_null() {
                    pat = pat.wrapping_add(1) | 1;
                    fill(p, size, pat);
                    let _ = to_remote[i % n_remote].send(Block(p, size, pat));
                }
            }
            2 => {
                let size = (op as usize * 67 % 16384) + 1;
                let p = rimalloc::malloc(size);
                if !p.is_null() {
                    pat = pat.wrapping_add(1) | 1;
                    fill(p, size, pat);
                    local.push(Block(p, size, pat));
                }
            }
            3 => {
                if let Ok(b) = back_rx.try_recv() {
                    check(&b);
                    rimalloc::free(b.0);
                }
            }
            _ => {
                if !local.is_empty() {
                    let b = local.swap_remove(op as usize % local.len());
                    check(&b);
                    rimalloc::free(b.0);
                }
            }
        }
    }

    drop(to_remote);
    for h in handles {
        h.join().unwrap();
    }
    while let Ok(b) = back_rx.recv() {
        check(&b);
        rimalloc::free(b.0);
    }
    for b in local.drain(..) {
        check(&b);
        rimalloc::free(b.0);
    }
    rimalloc::collect(false);
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

fn program(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    (0..len).map(|_| splitmix64(&mut state) as u8).collect()
}

#[test]
fn mt_ops_programs() {
    for seed in 0..8u64 {
        run_program(&program(seed, 512));
    }
}

#[test]
fn mt_ops_long_program() {
    run_program(&program(0xdead_beef, 4096));
}
