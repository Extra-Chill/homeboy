use std::hint::black_box;

pub fn tiny_workload() -> u64 {
    black_box(41u64).wrapping_add(black_box(1u64))
}
