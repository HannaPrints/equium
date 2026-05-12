// Equihash 96,5 BLAKE2b leaf-generation kernel (CUDA C++ port).
//
// Bypasses the Vulkan/SPIR-V code path entirely so we don't trip
// NVIDIA's libnvidia-glvkspirv.so bug on driver branches 555–575.
// Algorithm matches `clients/gpu-miner/src/shader_ref.rs::kernel_digest`
// byte-for-byte; the cargo test
// `shader_ref::tests::leaves_function_matches_reference_at_full_width`
// is the authoritative reference for "does this kernel compute the
// right thing?".
//
// Per-thread work mirrors the WGSL kernel exactly:
//   1. h_init = IV ^ parameter_block  (digest_len, fanout, depth,
//      personalization tail = "ZcashPoW" + n_le + k_le)
//   2. Build a 128-byte message block = [input(81) || nonce(32) ||
//      call_idx LE(4) || pad(11)]
//   3. One BLAKE2b compression with t = 117 and final flag = 0xFFFFFFFF
//   4. Truncate the 64-byte digest to 60 bytes, emit five 12-byte leaves
//
// CUDA gets native u64 (unlike WGSL's vec2<u32> emulation), so this
// version is much shorter than leaves.wgsl — but produces the same
// bits.

#include <cuda_runtime.h>
#include <cstdint>

extern "C" {

// BLAKE2b initialization vector.
__device__ __constant__ uint64_t IV[8] = {
    0x6a09e667f3bcc908ULL, 0xbb67ae8584caa73bULL,
    0x3c6ef372fe94f82bULL, 0xa54ff53a5f1d36f1ULL,
    0x510e527fade682d1ULL, 0x9b05688c2b3e6c1fULL,
    0x1f83d9abfb41bd6bULL, 0x5be0cd19137e2179ULL,
};

// Message word permutation schedule for BLAKE2b's 12 rounds. Last
// two rounds repeat 0 and 1.
__device__ __constant__ uint8_t SIGMA[12][16] = {
    {0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15},
    {14,10,4,8,9,15,13,6,1,12,0,2,11,7,5,3},
    {11,8,12,0,5,2,15,13,10,14,3,6,7,1,9,4},
    {7,9,3,1,13,12,11,14,2,6,5,10,4,0,15,8},
    {9,0,5,7,2,4,10,15,14,1,11,12,6,8,3,13},
    {2,12,6,10,0,11,8,3,4,13,7,5,15,14,1,9},
    {12,5,1,15,14,13,4,10,0,7,6,3,9,2,8,11},
    {13,11,7,14,12,1,3,9,5,0,15,4,8,6,2,10},
    {6,15,14,9,11,3,0,8,12,2,13,7,1,4,10,5},
    {10,2,8,4,7,6,1,5,15,11,9,14,3,12,13,0},
    {0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15},
    {14,10,4,8,9,15,13,6,1,12,0,2,11,7,5,3},
};

// BLAKE2b's G function — quarter round on 4 of the working vars.
__device__ __forceinline__ void G(
    uint64_t &a, uint64_t &b, uint64_t &c, uint64_t &d,
    uint64_t x, uint64_t y
) {
    a = a + b + x;
    d = (d ^ a);
    d = (d >> 32) | (d << 32);   // rotr 32
    c = c + d;
    b = b ^ c;
    b = (b >> 24) | (b << 40);   // rotr 24
    a = a + b + y;
    d = d ^ a;
    d = (d >> 16) | (d << 48);   // rotr 16
    c = c + d;
    b = b ^ c;
    b = (b >> 63) | (b << 1);    // rotr 63
}

// Single-block BLAKE2b compression with final flag asserted. t is
// the count of input bytes processed (always 117 for our workload —
// 81 input + 32 nonce + 4 call_idx).
__device__ __forceinline__ void compress(uint64_t h[8], const uint64_t m[16], uint64_t t) {
    uint64_t v[16];
    #pragma unroll
    for (int i = 0; i < 8; i++) v[i] = h[i];
    v[8]  = IV[0];
    v[9]  = IV[1];
    v[10] = IV[2];
    v[11] = IV[3];
    v[12] = IV[4] ^ t;
    v[13] = IV[5];
    v[14] = IV[6] ^ 0xFFFFFFFFFFFFFFFFULL;  // final block flag
    v[15] = IV[7];

    #pragma unroll
    for (int r = 0; r < 12; r++) {
        const uint8_t *s = SIGMA[r];
        G(v[0], v[4], v[8],  v[12], m[s[0]],  m[s[1]]);
        G(v[1], v[5], v[9],  v[13], m[s[2]],  m[s[3]]);
        G(v[2], v[6], v[10], v[14], m[s[4]],  m[s[5]]);
        G(v[3], v[7], v[11], v[15], m[s[6]],  m[s[7]]);
        G(v[0], v[5], v[10], v[15], m[s[8]],  m[s[9]]);
        G(v[1], v[6], v[11], v[12], m[s[10]], m[s[11]]);
        G(v[2], v[7], v[8],  v[13], m[s[12]], m[s[13]]);
        G(v[3], v[4], v[9],  v[14], m[s[14]], m[s[15]]);
    }

    #pragma unroll
    for (int i = 0; i < 8; i++) h[i] ^= v[i] ^ v[i + 8];
}

// Pack 8 bytes from a byte buffer into a little-endian u64 word.
// Used to build the 128-byte BLAKE2b message block from the
// concatenated input + nonce + call_idx.
__device__ __forceinline__ uint64_t pack_le(const uint8_t *p) {
    return ((uint64_t)p[0])       | ((uint64_t)p[1] << 8)  |
           ((uint64_t)p[2] << 16) | ((uint64_t)p[3] << 24) |
           ((uint64_t)p[4] << 32) | ((uint64_t)p[5] << 40) |
           ((uint64_t)p[6] << 48) | ((uint64_t)p[7] << 56);
}

// One BLAKE2b call per thread, emits LEAVES_PER_CALL=5 12-byte leaves.
// Uniform layout matches the WGSL kernel's `Params` exactly:
//   bytes 0..16    personalization ("ZcashPoW" + n_le + k_le)
//   bytes 16..32   cfg [digest_len=60, n_leaves, _, _]
//   bytes 32..128  input padded to 96  (only first 81 are read)
//   bytes 128..160 nonce (32 bytes)
__global__ void leaves_kernel(
    const uint8_t *__restrict__ params,
    uint8_t *__restrict__ out,           // n_leaves * 12 bytes
    uint32_t n_leaves
) {
    const uint32_t call_idx = blockIdx.x * blockDim.x + threadIdx.x;
    const uint32_t leaves_per_call = 5;
    const uint32_t n_calls = (n_leaves + leaves_per_call - 1) / leaves_per_call;
    if (call_idx >= n_calls) return;

    // Build h = IV ^ parameter_block.
    //   param[0]      = digest_len (60) | fanout(1)<<16 | depth(1)<<24
    //   param[1..6]   = 0
    //   param[6..8]   = personalization (16 bytes = 2 u64s)
    uint64_t h[8];
    h[0] = IV[0] ^ ((uint64_t)60u | (1ULL << 16) | (1ULL << 24));
    h[1] = IV[1];
    h[2] = IV[2];
    h[3] = IV[3];
    h[4] = IV[4];
    h[5] = IV[5];
    h[6] = IV[6] ^ pack_le(&params[0]);   // "ZcashPo" + "W"
    h[7] = IV[7] ^ pack_le(&params[8]);   // n_le (4) + k_le (4)

    // Build the 128-byte message block.
    //   bytes 0..81    input
    //   bytes 81..113  nonce
    //   bytes 113..117 call_idx LE
    //   bytes 117..128 zero pad
    uint8_t mbytes[128];
    #pragma unroll
    for (int i = 0; i < 81; i++) mbytes[i] = params[32 + i];
    #pragma unroll
    for (int i = 0; i < 32; i++) mbytes[81 + i] = params[128 + i];
    mbytes[113] = (uint8_t)(call_idx       & 0xFF);
    mbytes[114] = (uint8_t)((call_idx >> 8) & 0xFF);
    mbytes[115] = (uint8_t)((call_idx >> 16) & 0xFF);
    mbytes[116] = (uint8_t)((call_idx >> 24) & 0xFF);
    #pragma unroll
    for (int i = 117; i < 128; i++) mbytes[i] = 0;

    uint64_t m[16];
    #pragma unroll
    for (int i = 0; i < 16; i++) m[i] = pack_le(&mbytes[i * 8]);

    compress(h, m, 117ULL);

    // Emit up to 5 12-byte leaves from the 60-byte digest.
    const uint32_t base = call_idx * leaves_per_call;
    #pragma unroll
    for (int k = 0; k < 5; k++) {
        const uint32_t leaf = base + k;
        if (leaf >= n_leaves) break;
        const uint32_t out_off = leaf * 12;
        const uint32_t in_off  = k * 12;
        #pragma unroll
        for (int b = 0; b < 12; b++) {
            // The 60-byte digest lives in h[0..8] as little-endian
            // u64s. Pick the right byte: byte `i` of the digest is
            // (h[i / 8] >> ((i % 8) * 8)) & 0xFF.
            const uint32_t i = in_off + b;
            out[out_off + b] = (uint8_t)((h[i / 8] >> ((i % 8) * 8)) & 0xFFu);
        }
    }
}

} // extern "C"
