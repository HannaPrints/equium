// Equihash 96,5 Wagner pipeline (CUDA C++ port).
//
// Mirrors rounds.wgsl's four entry points one-to-one:
//   * init_rows     — leaves (12 bytes each) → rows (35 u32 each)
//   * count_buckets — atomic histogram + scatter into a bounded
//                     bucket-head table
//   * pair_emit     — pair within buckets, distinct-indices check,
//                     XOR-trim hash, canonical-concat indices
//   * solution_scan — surviving rows with all-zero hash → solutions
//
// CUDA has native u64 + 32-bit atomics so the code is shorter than
// the WGSL version, but the algorithm is byte-for-byte the same.
// Correctness lives in `clients/gpu-miner/src/shader_ref.rs::round_kernel`
// — `verify-rounds` checks this CUDA path against that CPU reference
// at full (96, 5) width.

#include <cuda_runtime.h>
#include <cstdint>

extern "C" {

#define HASH_WORDS     3u
#define INDICES_MAX    32u
#define ROW_WORDS      35u
#define MAX_PER_BUCKET 16u

struct Params {
    uint32_t n_rows;
    uint32_t max_out_rows;
    uint32_t indices_count_in;
    uint32_t _pad;
};

// Bucket id = first 2 hash bytes (byte0 << 8 | byte1). Matches
// shader_ref::extract_bucket. The exact bit-packing is arbitrary
// as long as count_buckets + pair_emit agree.
__device__ __forceinline__ uint32_t extract_bucket(const uint32_t *row) {
    uint32_t w0 = row[0];
    uint32_t b0 = w0 & 0xFFu;
    uint32_t b1 = (w0 >> 8u) & 0xFFu;
    return (b0 << 8u) | b1;
}

// Expand 12-byte leaves into 35-u32 rows. Each row gets its 3-word
// hash + leaf index in indices[0]; the rest of the indices array
// is read-only-uninitialized — the round loop only reads up to
// indices_count_in entries, which is 1 in round 0.
__global__ void init_rows(
    const Params *__restrict__ params,
    const uint32_t *__restrict__ leaves,   // n_leaves * 3 u32
    uint32_t *__restrict__ rows_out
) {
    uint32_t i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= params->n_rows) return;
    uint32_t leaf_base = i * 3u;
    uint32_t row_base  = i * ROW_WORDS;
    rows_out[row_base + 0u] = leaves[leaf_base + 0u];
    rows_out[row_base + 1u] = leaves[leaf_base + 1u];
    rows_out[row_base + 2u] = leaves[leaf_base + 2u];
    rows_out[row_base + HASH_WORDS] = i;
}

// Histogram + scatter. atomicAdd returns the old value, which is
// the slot in bucket_slots we get to write into. Overflow past
// MAX_PER_BUCKET silently drops the row (Poisson(2) tail at full
// width makes this astronomically rare).
__global__ void count_buckets(
    const Params *__restrict__ params,
    const uint32_t *__restrict__ rows_in,
    uint32_t *__restrict__ bucket_counts,
    uint32_t *__restrict__ bucket_slots
) {
    uint32_t i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= params->n_rows) return;
    uint32_t bucket = extract_bucket(&rows_in[i * ROW_WORDS]);
    uint32_t slot = atomicAdd(&bucket_counts[bucket], 1u);
    if (slot < MAX_PER_BUCKET) {
        bucket_slots[bucket * MAX_PER_BUCKET + slot] = i;
    }
}

// O(n²) disjoint-indices check, identical to shader_ref's version.
// Indices live in canonical-tree order (not sorted) so we can't
// merge-shortcut.
__device__ __forceinline__ bool distinct_indices(
    const uint32_t *a, const uint32_t *b, uint32_t n
) {
    for (uint32_t i = 0u; i < n; ++i) {
        uint32_t x = a[i];
        for (uint32_t j = 0u; j < n; ++j) {
            if (x == b[j]) return false;
        }
    }
    return true;
}

// One thread per input row. Walks its bucket-slot list for peers
// with strictly larger row index (so each pair is emitted exactly
// once), checks distinct indices, XORs + trims the hash, concats
// the index lists in canonical order, atomically reserves an
// output slot.
__global__ void pair_emit(
    const Params *__restrict__ params,
    const uint32_t *__restrict__ rows_in,
    uint32_t *__restrict__ rows_out,
    const uint32_t *__restrict__ bucket_counts,
    const uint32_t *__restrict__ bucket_slots,
    uint32_t *__restrict__ out_count
) {
    uint32_t i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= params->n_rows) return;

    const uint32_t *row_i = &rows_in[i * ROW_WORDS];
    uint32_t bucket = extract_bucket(row_i);
    uint32_t cnt = min(bucket_counts[bucket], MAX_PER_BUCKET);
    uint32_t base = bucket * MAX_PER_BUCKET;

    const uint32_t n = params->indices_count_in;
    const uint32_t *i_idx = row_i + HASH_WORDS;
    uint32_t max_out = params->max_out_rows;

    for (uint32_t s = 0u; s < cnt; ++s) {
        uint32_t j = bucket_slots[base + s];
        if (j <= i) continue;
        const uint32_t *row_j = &rows_in[j * ROW_WORDS];
        const uint32_t *j_idx = row_j + HASH_WORDS;
        if (!distinct_indices(i_idx, j_idx, n)) continue;

        uint32_t out_idx = atomicAdd(out_count, 1u);
        if (out_idx >= max_out) break;

        // XOR-and-trim. cbytes=2 always for (96,5): shift left by
        // 2 bytes. Trailing bytes propagate zero by induction.
        uint32_t h0 = row_i[0] ^ row_j[0];
        uint32_t h1 = row_i[1] ^ row_j[1];
        uint32_t h2 = row_i[2] ^ row_j[2];
        uint32_t *row_o = &rows_out[out_idx * ROW_WORDS];
        row_o[0] = (h0 >> 16) | (h1 << 16);
        row_o[1] = (h1 >> 16) | (h2 << 16);
        row_o[2] = h2 >> 16;

        // Canonical concat — smaller-by-indices[0] first. Matches
        // equihash_core::solver::concat_canonical.
        uint32_t i0 = i_idx[0];
        uint32_t j0 = j_idx[0];
        uint32_t *out_idx_base = row_o + HASH_WORDS;
        if (i0 < j0) {
            for (uint32_t k = 0u; k < n; ++k) {
                out_idx_base[k]     = i_idx[k];
                out_idx_base[n + k] = j_idx[k];
            }
        } else {
            for (uint32_t k = 0u; k < n; ++k) {
                out_idx_base[k]     = j_idx[k];
                out_idx_base[n + k] = i_idx[k];
            }
        }
    }
}

// Rows surviving 5 Wagner rounds whose 2-byte hash is all-zero are
// candidate solutions. Copy their 32-index lists into rows_out at
// freshly-allocated slots.
__global__ void solution_scan(
    const Params *__restrict__ params,
    const uint32_t *__restrict__ rows_in,
    uint32_t *__restrict__ rows_out,
    uint32_t *__restrict__ out_count
) {
    uint32_t i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= params->n_rows) return;
    const uint32_t *row = &rows_in[i * ROW_WORDS];
    if (row[0] != 0u || row[1] != 0u || row[2] != 0u) return;

    uint32_t slot = atomicAdd(out_count, 1u);
    if (slot >= params->max_out_rows) return;
    uint32_t *row_o = &rows_out[slot * ROW_WORDS];
    row_o[0] = 0u;
    row_o[1] = 0u;
    row_o[2] = 0u;
    for (uint32_t k = 0u; k < INDICES_MAX; ++k) {
        row_o[HASH_WORDS + k] = row[HASH_WORDS + k];
    }
}

} // extern "C"
