// One Wagner round for Equihash 96,5 — GPU-shape implementation.
//
// Mirrors clients/gpu-miner/src/shader_ref.rs::round_kernel one-to-one.
// The Rust port is validated round-by-round against
// equihash_core::solver::round at full (96, 5) width by `cargo test`.
// As long as this file matches the Rust port the GPU path is correct,
// barring driver bugs.
//
// Pipeline (two dispatches per round):
//   1. count_buckets  — histogram first-2-byte buckets, scatter into a
//                       bounded-per-bucket slot table.
//   2. pair_emit      — for each input row, iterate its bucket; for
//                       each higher-indexed peer with disjoint indices,
//                       emit (XOR-trim hash, canonical-concat indices)
//                       into the output buffer.
//
// Row layout (35 u32 = 140 bytes):
//   words 0..3   hash (12 bytes max, LE-packed)
//   words 3..35  indices (up to 32 u32; only first `indices_count_in`
//                are meaningful)
//
// MAX_PER_BUCKET=16 caps the bucket-head table. For (96,5) with
// 131,072 rows in 65,536 buckets the bucket size is Poisson(2);
// overflow at MAX=16 is astronomically rare and only loses pairs from
// that one bucket — never corrupts state.

const HASH_WORDS: u32 = 3u;
const INDICES_MAX: u32 = 32u;
const ROW_WORDS: u32 = 35u;
const NUM_BUCKETS: u32 = 65536u;
const MAX_PER_BUCKET: u32 = 16u;

struct Params {
    n_rows: u32,
    max_out_rows: u32,
    indices_count_in: u32,
    _pad: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> rows_in: array<u32>;
@group(0) @binding(2) var<storage, read_write> rows_out: array<u32>;
@group(0) @binding(3) var<storage, read_write> bucket_counts: array<atomic<u32>>;
@group(0) @binding(4) var<storage, read_write> bucket_slots: array<u32>;
@group(0) @binding(5) var<storage, read_write> out_count: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read> leaves: array<u32>;

fn read_hash_word(row: u32, w: u32) -> u32 {
    return rows_in[row * ROW_WORDS + w];
}

fn read_index_in(row: u32, idx: u32) -> u32 {
    return rows_in[row * ROW_WORDS + HASH_WORDS + idx];
}

// Bucket id from the first 2 hash bytes. The exact bit-packing scheme
// is arbitrary — only consistency between count_buckets and pair_emit
// matters. Matches shader_ref::extract_bucket: (byte0 << 8) | byte1.
fn extract_bucket(row: u32) -> u32 {
    let w0 = read_hash_word(row, 0u);
    let b0 = w0 & 0xFFu;
    let b1 = (w0 >> 8u) & 0xFFu;
    return (b0 << 8u) | b1;
}

// Pipeline-overridable workgroup size. Host picks per vendor (NVIDIA
// 128, AMD 64, Apple 64, Intel 32) to match warp/wavefront width.
override WG_SIZE: u32 = 64;

// Expand GPU-side leaves (3 u32 per leaf) into row-format rows_out:
// each row gets the 3 hash words + its leaf index as indices[0]. Run
// once after the leaves kernel and before round 0's count_buckets.
@compute @workgroup_size(WG_SIZE)
fn init_rows(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_rows) { return; }
    let leaf_base = i * 3u;
    let row_base = i * ROW_WORDS;
    rows_out[row_base + 0u] = leaves[leaf_base + 0u];
    rows_out[row_base + 1u] = leaves[leaf_base + 1u];
    rows_out[row_base + 2u] = leaves[leaf_base + 2u];
    rows_out[row_base + HASH_WORDS] = i;
}

// Surviving rows after the final Wagner round whose hash is all-zero
// are candidate solutions. Each one's 32-index list is copied into
// rows_out at a freshly-allocated solution slot (atomic on out_count).
@compute @workgroup_size(WG_SIZE)
fn solution_scan(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_rows) { return; }
    let base = i * ROW_WORDS;
    let h0 = rows_in[base + 0u];
    let h1 = rows_in[base + 1u];
    let h2 = rows_in[base + 2u];
    if (h0 != 0u || h1 != 0u || h2 != 0u) { return; }

    let slot = atomicAdd(&out_count[0], 1u);
    if (slot >= params.max_out_rows) { return; }

    let out_base = slot * ROW_WORDS;
    rows_out[out_base + 0u] = 0u;
    rows_out[out_base + 1u] = 0u;
    rows_out[out_base + 2u] = 0u;
    for (var k: u32 = 0u; k < INDICES_MAX; k = k + 1u) {
        rows_out[out_base + HASH_WORDS + k] = rows_in[base + HASH_WORDS + k];
    }
}

@compute @workgroup_size(WG_SIZE)
fn count_buckets(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_rows) { return; }
    let bucket = extract_bucket(i);
    let slot = atomicAdd(&bucket_counts[bucket], 1u);
    if (slot < MAX_PER_BUCKET) {
        bucket_slots[bucket * MAX_PER_BUCKET + slot] = i;
    }
}

// O(n²) disjoint-indices check. Mirrors the upstream verifier — the
// indices are stored in canonical-tree order, not sorted, so we cannot
// shortcut to a merge.
fn distinct_indices(a: u32, b: u32) -> bool {
    let n = params.indices_count_in;
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let x = read_index_in(a, i);
        for (var j: u32 = 0u; j < n; j = j + 1u) {
            if (x == read_index_in(b, j)) {
                return false;
            }
        }
    }
    return true;
}

@compute @workgroup_size(WG_SIZE)
fn pair_emit(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_rows) { return; }

    let bucket = extract_bucket(i);
    let cnt = min(atomicLoad(&bucket_counts[bucket]), MAX_PER_BUCKET);
    let base = bucket * MAX_PER_BUCKET;

    let n = params.indices_count_in;

    for (var s: u32 = 0u; s < cnt; s = s + 1u) {
        let j = bucket_slots[base + s];
        if (j <= i) { continue; }
        if (!distinct_indices(i, j)) { continue; }

        // Reserve an output slot. If we'd overflow, abandon this pair
        // and the rest of this row's bucket — we've still incremented
        // out_count by one, so the host can detect overflow on
        // readback and resize for the next attempt.
        let out_idx = atomicAdd(&out_count[0], 1u);
        if (out_idx >= params.max_out_rows) {
            break;
        }

        // XOR-and-trim. cbytes=2 always for (96,5): shift the 12-byte
        // hash left by 2 bytes. Trailing bytes propagate zero by
        // induction because the round-0 input has none.
        let h0 = read_hash_word(i, 0u) ^ read_hash_word(j, 0u);
        let h1 = read_hash_word(i, 1u) ^ read_hash_word(j, 1u);
        let h2 = read_hash_word(i, 2u) ^ read_hash_word(j, 2u);

        let out_base = out_idx * ROW_WORDS;
        rows_out[out_base + 0u] = (h0 >> 16u) | (h1 << 16u);
        rows_out[out_base + 1u] = (h1 >> 16u) | (h2 << 16u);
        rows_out[out_base + 2u] = h2 >> 16u;

        // Canonical concat: subtree with smaller indices[0] goes first.
        // Matches `equihash_core::solver::concat_canonical`.
        let i_idx0 = read_index_in(i, 0u);
        let j_idx0 = read_index_in(j, 0u);
        if (i_idx0 < j_idx0) {
            for (var k: u32 = 0u; k < n; k = k + 1u) {
                rows_out[out_base + HASH_WORDS + k] = read_index_in(i, k);
                rows_out[out_base + HASH_WORDS + n + k] = read_index_in(j, k);
            }
        } else {
            for (var k: u32 = 0u; k < n; k = k + 1u) {
                rows_out[out_base + HASH_WORDS + k] = read_index_in(j, k);
                rows_out[out_base + HASH_WORDS + n + k] = read_index_in(i, k);
            }
        }
    }
}
