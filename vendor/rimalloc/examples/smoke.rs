fn main() {
    // basic alloc/free
    let p = rimalloc::malloc(42);
    assert!(!p.is_null());
    assert!(rimalloc::usable_size(p) >= 42);
    unsafe { std::ptr::write_bytes(p, 0xab, 42) };
    rimalloc::free(p);

    // many sizes
    let mut ptrs = Vec::new();
    for i in 0..10000usize {
        let size = (i % 4000) + 1;
        let p = rimalloc::malloc(size);
        assert!(!p.is_null(), "alloc {size}");
        unsafe { std::ptr::write_bytes(p, (i & 0xff) as u8, size) };
        ptrs.push((p, size, (i & 0xff) as u8));
    }
    for (p, size, v) in &ptrs {
        unsafe {
            for j in 0..*size {
                assert_eq!(*p.add(j), *v, "corruption at {j} of {size}");
            }
        }
        rimalloc::free(*p);
    }

    // zalloc
    let p = rimalloc::zalloc(1024 * 1024);
    unsafe {
        for j in 0..1024 * 1024 {
            assert_eq!(*p.add(j), 0);
        }
    }
    rimalloc::free(p);

    // large + huge
    for size in [100_000, 1_000_000, 20_000_000, 40_000_000] {
        let p = rimalloc::malloc(size);
        assert!(!p.is_null());
        unsafe {
            *p = 1;
            *p.add(size - 1) = 2;
        }
        rimalloc::free(p);
    }

    // aligned
    for align in [16usize, 64, 256, 4096, 65536, 1 << 20] {
        let p = rimalloc::malloc_aligned(1000, align);
        assert!(!p.is_null());
        assert_eq!(p.addr() % align, 0, "align {align}");
        unsafe { std::ptr::write_bytes(p, 0xcd, 1000) };
        assert!(rimalloc::usable_size(p) >= 1000);
        rimalloc::free(p);
    }

    // realloc
    let mut p = rimalloc::malloc(10);
    unsafe { std::ptr::write_bytes(p, 7, 10) };
    p = rimalloc::realloc(p, 100_000);
    unsafe {
        for j in 0..10 {
            assert_eq!(*p.add(j), 7);
        }
    }
    rimalloc::free(p);

    // cross-thread free
    let mut handles = Vec::new();
    let (tx, rx) = std::sync::mpsc::channel::<(usize, usize)>();
    for _ in 0..4 {
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..1000usize {
                let size = (i % 2048) + 8;
                let p = rimalloc::malloc(size);
                unsafe { std::ptr::write_bytes(p, 0x5a, size) };
                tx.send((p.addr(), size)).unwrap();
            }
        }));
    }
    drop(tx);
    let mut received = Vec::new();
    while let Ok((addr, size)) = rx.recv() {
        received.push((addr, size));
    }
    for h in handles {
        h.join().unwrap();
    }
    for (addr, size) in received {
        let p = std::ptr::with_exposed_provenance_mut::<u8>(addr);
        unsafe {
            for j in 0..size {
                assert_eq!(*p.add(j), 0x5a);
            }
        }
        rimalloc::free(p);
    }
    rimalloc::collect(true);
    rimalloc::stats_print();
    println!("rimalloc smoke: OK");
}
