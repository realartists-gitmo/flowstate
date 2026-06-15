use rimalloc::Rimalloc;

#[global_allocator]
static ALLOC: Rimalloc = Rimalloc;

fn main() {
    // exercise Vec/String/HashMap through the global allocator
    let mut v: Vec<u64> = Vec::new();
    for i in 0..1_000_000 {
        v.push(i);
    }
    assert_eq!(v.iter().sum::<u64>(), 999_999 * 1_000_000 / 2);

    let mut m = std::collections::HashMap::new();
    for i in 0..100_000u64 {
        m.insert(i, i.to_string());
    }
    assert_eq!(m[&99_999], "99999");
    drop(m);

    let handles: Vec<_> = (0..8)
        .map(|t| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                for i in 0..50_000usize {
                    buf.push(vec![t as u8; (i % 512) + 1]);
                    if i % 3 == 0 {
                        buf.swap_remove(buf.len() / 2);
                    }
                }
                buf.len()
            })
        })
        .collect();
    let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    println!("global allocator OK ({total} survivors)");
}
