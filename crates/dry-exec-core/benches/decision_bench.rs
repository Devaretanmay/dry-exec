use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use dry_exec_core::decision::{evaluate, DeltaSummary, Environment};
use dry_exec_core::delta::{PageMutation, StateDelta};
use std::hint::black_box;

/// Page count whose 4KB granularity accounts for roughly 100MB of mutation volume.
const HUNDRED_MEGABYTE_PAGES: usize = 25_600;

/// Scalar summary of a state delta reporting roughly 100MB of mutated memory.
fn hundred_megabyte_summary() -> DeltaSummary {
    let page = PageMutation {
        page_index: 0,
        page_address: 0,
        deltas: Vec::new(),
    };
    let delta = StateDelta {
        memory_mutations: vec![page; HUNDRED_MEGABYTE_PAGES],
        total_bytes_mutated: HUNDRED_MEGABYTE_PAGES * 4096,
        ..Default::default()
    };
    DeltaSummary::from(&delta)
}

fn bench_decision_evaluation(c: &mut Criterion) {
    let environment = Environment::default();
    let summary = hundred_megabyte_summary();

    let mut group = c.benchmark_group("system_one_decision");
    group.bench_function(BenchmarkId::new("evaluate", "100MB_state_delta"), |b| {
        b.iter(|| evaluate(black_box(&summary), black_box(&environment)));
    });
    group.finish();
}

/// Scalar routing cannot scale with state size: both summaries must measure alike.
fn bench_decision_size_invariance(c: &mut Criterion) {
    let environment = Environment::default();
    let large = hundred_megabyte_summary();
    let empty = DeltaSummary::default();

    let mut group = c.benchmark_group("system_one_decision_size_invariance");
    for (label, summary) in [("100MB_state_delta", large), ("empty_state_delta", empty)] {
        group.bench_with_input(BenchmarkId::new("evaluate", label), &summary, |b, input| {
            b.iter(|| evaluate(black_box(input), black_box(&environment)));
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_decision_evaluation,
    bench_decision_size_invariance
);
criterion_main!(benches);
