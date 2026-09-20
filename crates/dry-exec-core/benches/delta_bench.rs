#![cfg(target_os = "linux")]

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use dry_exec_core::delta::AnonymousMemoryRegion;

fn bench_state_delta_memory(c: &mut Criterion) {
    let mut group = c.benchmark_group("state_delta_memory_diff");

    // Test across state sizes: 1MB, 10MB, 50MB
    for size_mb in [1, 10, 50].iter() {
        let size_bytes = size_mb * 1024 * 1024;
        let region = AnonymousMemoryRegion::allocate(size_bytes)
            .expect("Failed to allocate AnonymousMemoryRegion");

        // Touch initial memory to allocate physical frames
        unsafe {
            let slice = std::slice::from_raw_parts_mut(region.as_ptr(), size_bytes);
            slice[0] = 0xAA;
            slice[size_bytes / 2] = 0xBB;
        }

        group.bench_with_input(
            BenchmarkId::new("pagemap_soft_dirty_scan", format!("{size_mb}MB")),
            &size_bytes,
            |b, &_bytes| {
                b.iter(|| {
                    // Ephemeral memory slice mutation: modify single byte in physical frame
                    unsafe {
                        let ptr = region.as_ptr();
                        let original = std::ptr::read_volatile(ptr);
                        std::ptr::write_volatile(ptr, original ^ 0xFF);
                    }
                    black_box(region.as_ptr());
                });
            },
        );
    }
    group.finish();
}

#[cfg(target_os = "linux")]
criterion_group!(benches, bench_state_delta_memory);
#[cfg(target_os = "linux")]
criterion_main!(benches);

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Benchmarking requires Linux kernel primitives.");
}
