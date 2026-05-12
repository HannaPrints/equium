// Equihash 96,5 leaf-generation kernel.
//
// One invocation = one BLAKE2b call producing 5 leaves of 12 bytes each
// (n=96, k=5 → digest_length=60, indices_per_hash=5).
//
// Inputs (uniform):
//   * personalization: "ZcashPoW" || n_le || k_le (16 bytes, BLAKE2b
//     param-block bytes 48..64)
//   * digest_len: 60 for (96,5). Goes into param-block byte 0.
//   * input: the 81-byte Equihash I-block (Equium-v1 + challenge + miner
//     + height)
//   * nonce: 32-byte miner-chosen nonce
//   * n_leaves: how many leaves to actually write (lets us bound work)
//
// Per invocation:
//   1. Compute h_init = IV ^ parameter_block_as_u64s.
//   2. Pack [input(81) || nonce(32) || index_le(4) || pad to 128] into
//      a single 128-byte message block.
//   3. Run one BLAKE2b compression with the finalization flag set
//      (the total input is 81+32+4 = 117 < 128 bytes; no full block
//      ever gets compressed before this).
//   4. Truncate the 64-byte digest to digest_len bytes (60) and emit
//      five 12-byte leaves into the output buffer.

// ---- u64 = vec2<u32> arithmetic ----

fn u64_add(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.x + b.x;
    let carry = select(0u, 1u, lo < a.x);
    let hi = a.y + b.y + carry;
    return vec2<u32>(lo, hi);
}

fn u64_xor(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x ^ b.x, a.y ^ b.y);
}

// 64-bit right-rotate. BLAKE2b only uses rotations {32, 24, 16, 63}.
fn u64_rotr(a: vec2<u32>, n: u32) -> vec2<u32> {
    if (n == 32u) {
        return vec2<u32>(a.y, a.x);
    }
    if (n < 32u) {
        let lo = (a.x >> n) | (a.y << (32u - n));
        let hi = (a.y >> n) | (a.x << (32u - n));
        return vec2<u32>(lo, hi);
    }
    // n in {33..63}: swap halves first.
    let m = n - 32u;
    let swapped = vec2<u32>(a.y, a.x);
    let lo = (swapped.x >> m) | (swapped.y << (32u - m));
    let hi = (swapped.y >> m) | (swapped.x << (32u - m));
    return vec2<u32>(lo, hi);
}

// BLAKE2b initialization vector.
const IV0: vec2<u32> = vec2<u32>(0xf3bcc908u, 0x6a09e667u);
const IV1: vec2<u32> = vec2<u32>(0x84caa73bu, 0xbb67ae85u);
const IV2: vec2<u32> = vec2<u32>(0xfe94f82bu, 0x3c6ef372u);
const IV3: vec2<u32> = vec2<u32>(0x5f1d36f1u, 0xa54ff53au);
const IV4: vec2<u32> = vec2<u32>(0xade682d1u, 0x510e527fu);
const IV5: vec2<u32> = vec2<u32>(0x2b3e6c1fu, 0x9b05688cu);
const IV6: vec2<u32> = vec2<u32>(0xfb41bd6bu, 0x1f83d9abu);
const IV7: vec2<u32> = vec2<u32>(0x137e2179u, 0x5be0cd19u);

// Message schedule.
const SIGMA: array<array<u32, 16>, 12> = array<array<u32, 16>, 12>(
    array<u32, 16>(0u, 1u, 2u, 3u, 4u, 5u, 6u, 7u, 8u, 9u, 10u, 11u, 12u, 13u, 14u, 15u),
    array<u32, 16>(14u, 10u, 4u, 8u, 9u, 15u, 13u, 6u, 1u, 12u, 0u, 2u, 11u, 7u, 5u, 3u),
    array<u32, 16>(11u, 8u, 12u, 0u, 5u, 2u, 15u, 13u, 10u, 14u, 3u, 6u, 7u, 1u, 9u, 4u),
    array<u32, 16>(7u, 9u, 3u, 1u, 13u, 12u, 11u, 14u, 2u, 6u, 5u, 10u, 4u, 0u, 15u, 8u),
    array<u32, 16>(9u, 0u, 5u, 7u, 2u, 4u, 10u, 15u, 14u, 1u, 11u, 12u, 6u, 8u, 3u, 13u),
    array<u32, 16>(2u, 12u, 6u, 10u, 0u, 11u, 8u, 3u, 4u, 13u, 7u, 5u, 15u, 14u, 1u, 9u),
    array<u32, 16>(12u, 5u, 1u, 15u, 14u, 13u, 4u, 10u, 0u, 7u, 6u, 3u, 9u, 2u, 8u, 11u),
    array<u32, 16>(13u, 11u, 7u, 14u, 12u, 1u, 3u, 9u, 5u, 0u, 15u, 4u, 8u, 6u, 2u, 10u),
    array<u32, 16>(6u, 15u, 14u, 9u, 11u, 3u, 0u, 8u, 12u, 2u, 13u, 7u, 1u, 4u, 10u, 5u),
    array<u32, 16>(10u, 2u, 8u, 4u, 7u, 6u, 1u, 5u, 15u, 11u, 9u, 14u, 3u, 12u, 13u, 0u),
    array<u32, 16>(0u, 1u, 2u, 3u, 4u, 5u, 6u, 7u, 8u, 9u, 10u, 11u, 12u, 13u, 14u, 15u),
    array<u32, 16>(14u, 10u, 4u, 8u, 9u, 15u, 13u, 6u, 1u, 12u, 0u, 2u, 11u, 7u, 5u, 3u),
);

// ---- Uniform inputs ----

struct Params {
    // 16-byte personalization. Bytes 48..64 of the BLAKE2b param block.
    // `.xy` form the lo u64 (= "ZcashPoW"), `.zw` form the hi u64
    // (= n_le ++ k_le).
    personal: vec4<u32>,
    // .x = digest_len (60 for n=96), .y = n_leaves, .z .w = padding.
    cfg: vec4<u32>,

    // 81-byte I-block padded to 96 (6 × vec4). Trailing 15 bytes zero.
    input: array<vec4<u32>, 6>,

    // 32-byte nonce, 8 u32 LE across two vec4s.
    nonce: vec4<u32>,
    nonce_hi: vec4<u32>,
}

@group(0) @binding(0) var<uniform> params: Params;

// Output: tightly packed 12-byte leaves. Each leaf = 3 × u32 little-endian.
@group(0) @binding(1) var<storage, read_write> leaves: array<u32>;

// Compress one block into h. `t` is the total bytes counter (= 117 for
// our single-block case). `f` is 0xFFFFFFFFu when this is the final
// block, 0 otherwise.
fn compress(
    h: ptr<function, array<vec2<u32>, 8>>,
    block: array<vec2<u32>, 16>,
    t: vec2<u32>,
    f: u32,
) {
    var v: array<vec2<u32>, 16>;
    v[0] = (*h)[0]; v[1] = (*h)[1]; v[2] = (*h)[2]; v[3] = (*h)[3];
    v[4] = (*h)[4]; v[5] = (*h)[5]; v[6] = (*h)[6]; v[7] = (*h)[7];
    v[8] = IV0;  v[9] = IV1;  v[10] = IV2; v[11] = IV3;
    v[12] = u64_xor(IV4, t);
    v[13] = IV5;
    v[14] = vec2<u32>(IV6.x ^ f, IV6.y ^ f);
    v[15] = IV7;

    for (var r = 0u; r < 12u; r = r + 1u) {
        let s = SIGMA[r];

        // Column lanes: G(0,4,8,12), G(1,5,9,13), G(2,6,10,14), G(3,7,11,15)
        v[0]  = u64_add(u64_add(v[0],  v[4]),  block[s[0]]);
        v[12] = u64_rotr(u64_xor(v[12], v[0]),  32u);
        v[8]  = u64_add(v[8],  v[12]);
        v[4]  = u64_rotr(u64_xor(v[4],  v[8]),  24u);
        v[0]  = u64_add(u64_add(v[0],  v[4]),  block[s[1]]);
        v[12] = u64_rotr(u64_xor(v[12], v[0]),  16u);
        v[8]  = u64_add(v[8],  v[12]);
        v[4]  = u64_rotr(u64_xor(v[4],  v[8]),  63u);

        v[1]  = u64_add(u64_add(v[1],  v[5]),  block[s[2]]);
        v[13] = u64_rotr(u64_xor(v[13], v[1]),  32u);
        v[9]  = u64_add(v[9],  v[13]);
        v[5]  = u64_rotr(u64_xor(v[5],  v[9]),  24u);
        v[1]  = u64_add(u64_add(v[1],  v[5]),  block[s[3]]);
        v[13] = u64_rotr(u64_xor(v[13], v[1]),  16u);
        v[9]  = u64_add(v[9],  v[13]);
        v[5]  = u64_rotr(u64_xor(v[5],  v[9]),  63u);

        v[2]  = u64_add(u64_add(v[2],  v[6]),  block[s[4]]);
        v[14] = u64_rotr(u64_xor(v[14], v[2]),  32u);
        v[10] = u64_add(v[10], v[14]);
        v[6]  = u64_rotr(u64_xor(v[6],  v[10]), 24u);
        v[2]  = u64_add(u64_add(v[2],  v[6]),  block[s[5]]);
        v[14] = u64_rotr(u64_xor(v[14], v[2]),  16u);
        v[10] = u64_add(v[10], v[14]);
        v[6]  = u64_rotr(u64_xor(v[6],  v[10]), 63u);

        v[3]  = u64_add(u64_add(v[3],  v[7]),  block[s[6]]);
        v[15] = u64_rotr(u64_xor(v[15], v[3]),  32u);
        v[11] = u64_add(v[11], v[15]);
        v[7]  = u64_rotr(u64_xor(v[7],  v[11]), 24u);
        v[3]  = u64_add(u64_add(v[3],  v[7]),  block[s[7]]);
        v[15] = u64_rotr(u64_xor(v[15], v[3]),  16u);
        v[11] = u64_add(v[11], v[15]);
        v[7]  = u64_rotr(u64_xor(v[7],  v[11]), 63u);

        // Diagonal lanes.
        v[0]  = u64_add(u64_add(v[0],  v[5]),  block[s[8]]);
        v[15] = u64_rotr(u64_xor(v[15], v[0]),  32u);
        v[10] = u64_add(v[10], v[15]);
        v[5]  = u64_rotr(u64_xor(v[5],  v[10]), 24u);
        v[0]  = u64_add(u64_add(v[0],  v[5]),  block[s[9]]);
        v[15] = u64_rotr(u64_xor(v[15], v[0]),  16u);
        v[10] = u64_add(v[10], v[15]);
        v[5]  = u64_rotr(u64_xor(v[5],  v[10]), 63u);

        v[1]  = u64_add(u64_add(v[1],  v[6]),  block[s[10]]);
        v[12] = u64_rotr(u64_xor(v[12], v[1]),  32u);
        v[11] = u64_add(v[11], v[12]);
        v[6]  = u64_rotr(u64_xor(v[6],  v[11]), 24u);
        v[1]  = u64_add(u64_add(v[1],  v[6]),  block[s[11]]);
        v[12] = u64_rotr(u64_xor(v[12], v[1]),  16u);
        v[11] = u64_add(v[11], v[12]);
        v[6]  = u64_rotr(u64_xor(v[6],  v[11]), 63u);

        v[2]  = u64_add(u64_add(v[2],  v[7]),  block[s[12]]);
        v[13] = u64_rotr(u64_xor(v[13], v[2]),  32u);
        v[8]  = u64_add(v[8],  v[13]);
        v[7]  = u64_rotr(u64_xor(v[7],  v[8]),  24u);
        v[2]  = u64_add(u64_add(v[2],  v[7]),  block[s[13]]);
        v[13] = u64_rotr(u64_xor(v[13], v[2]),  16u);
        v[8]  = u64_add(v[8],  v[13]);
        v[7]  = u64_rotr(u64_xor(v[7],  v[8]),  63u);

        v[3]  = u64_add(u64_add(v[3],  v[4]),  block[s[14]]);
        v[14] = u64_rotr(u64_xor(v[14], v[3]),  32u);
        v[9]  = u64_add(v[9],  v[14]);
        v[4]  = u64_rotr(u64_xor(v[4],  v[9]),  24u);
        v[3]  = u64_add(u64_add(v[3],  v[4]),  block[s[15]]);
        v[14] = u64_rotr(u64_xor(v[14], v[3]),  16u);
        v[9]  = u64_add(v[9],  v[14]);
        v[4]  = u64_rotr(u64_xor(v[4],  v[9]),  63u);
    }

    (*h)[0] = u64_xor(u64_xor((*h)[0], v[0]), v[8]);
    (*h)[1] = u64_xor(u64_xor((*h)[1], v[1]), v[9]);
    (*h)[2] = u64_xor(u64_xor((*h)[2], v[2]), v[10]);
    (*h)[3] = u64_xor(u64_xor((*h)[3], v[3]), v[11]);
    (*h)[4] = u64_xor(u64_xor((*h)[4], v[4]), v[12]);
    (*h)[5] = u64_xor(u64_xor((*h)[5], v[5]), v[13]);
    (*h)[6] = u64_xor(u64_xor((*h)[6], v[6]), v[14]);
    (*h)[7] = u64_xor(u64_xor((*h)[7], v[7]), v[15]);
}

// Helper: read one u32 LE word from the packed input region. The
// `input` field is stored as 6 vec4<u32> (24 words total); we only use
// the first 21 (84 bytes ≥ our 81-byte I-block; trailing 3 bytes are
// zero).
fn read_input_word(i: u32) -> u32 {
    let v = params.input[i >> 2u];
    let lane = i & 3u;
    if (lane == 0u) { return v.x; }
    if (lane == 1u) { return v.y; }
    if (lane == 2u) { return v.z; }
    return v.w;
}

fn read_nonce_word(i: u32) -> u32 {
    if (i < 4u) {
        switch (i) {
            case 0u: { return params.nonce.x; }
            case 1u: { return params.nonce.y; }
            case 2u: { return params.nonce.z; }
            default: { return params.nonce.w; }
        }
    } else {
        switch (i - 4u) {
            case 0u: { return params.nonce_hi.x; }
            case 1u: { return params.nonce_hi.y; }
            case 2u: { return params.nonce_hi.z; }
            default: { return params.nonce_hi.w; }
        }
    }
}

// Pipeline-overridable workgroup size. Host code picks per GPU vendor:
//   NVIDIA → 128 (4 warps per group, good occupancy)
//   AMD    → 64  (one wavefront)
//   Apple  → 64
//   Intel  → 32
//   else   → 64 (current default)
override WG_SIZE: u32 = 64;

@compute @workgroup_size(WG_SIZE)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    // One invocation == one BLAKE2b call == five 12-byte leaves.
    let call_idx = gid.x;
    let n_leaves = params.cfg.y;
    let digest_len = params.cfg.x;
    let first_leaf = call_idx * 5u;
    if (first_leaf >= n_leaves) {
        return;
    }

    // h_init = IV ^ parameter_block. First 4 bytes of the param block
    // are [digest_len, 0, 1, 1] (fanout=1, depth=1). Bytes 4..48 are
    // zero. Bytes 48..64 are the personalization.
    var h: array<vec2<u32>, 8>;
    let p0_lo = digest_len | (1u << 16u) | (1u << 24u);
    h[0] = vec2<u32>(IV0.x ^ p0_lo, IV0.y);
    h[1] = IV1;
    h[2] = IV2;
    h[3] = IV3;
    h[4] = IV4;
    h[5] = IV5;
    h[6] = u64_xor(IV6, vec2<u32>(params.personal.x, params.personal.y));
    h[7] = u64_xor(IV7, vec2<u32>(params.personal.z, params.personal.w));

    // Build the single message block: input(81) || nonce(32) || idx(4) || pad(11)
    // = 128 bytes total. Lay out as 32 u32 words (32 × 4 = 128 bytes),
    // each pair of which forms one u64 lane of the message schedule.
    var w: array<u32, 32>;
    // Input (81 bytes = 20 full u32 words + 1 partial word of 1 byte).
    for (var i = 0u; i < 20u; i = i + 1u) {
        w[i] = read_input_word(i);
    }
    // The 21st u32 holds bytes 80..81 of the input in its low byte +
    // the first 3 bytes of the nonce in its upper bytes.
    let input_tail = read_input_word(20u) & 0x000000ffu;  // byte 80 only
    // Nonce starts at byte 81 of the block. 81 is not 4-aligned, so we
    // have to do unaligned packing.
    //
    // Block layout (bytes, 0-indexed):
    //   0..80   : input bytes 0..80
    //   81..112 : 32 nonce bytes
    //   113..116: 4-byte LE index counter
    //   117..127: zero padding
    //
    // u32 layout (each word holds 4 LE bytes, word i = bytes 4i..4i+4):
    //   words 0..19 : input bytes 0..80 (exact)
    //   word 20     : [input80, nonce0, nonce1, nonce2]
    //   word 21..27 : [nonce3..nonce30] in groups of 4
    //   word 28     : [nonce31, idx_b0, idx_b1, idx_b2]
    //   word 29     : [idx_b3, 0, 0, 0]
    //   word 30..31 : 0
    let n0 = read_nonce_word(0u);  // bytes 0..3 of nonce as little-endian u32
    let n1 = read_nonce_word(1u);
    let n2 = read_nonce_word(2u);
    let n3 = read_nonce_word(3u);
    let n4 = read_nonce_word(4u);
    let n5 = read_nonce_word(5u);
    let n6 = read_nonce_word(6u);
    let n7 = read_nonce_word(7u);

    // Pack [b80, nonce[0..3]] into word 20:
    // byte 0 = input byte 80, bytes 1..3 = nonce bytes 0,1,2
    w[20] = input_tail | ((n0 & 0x00ffffffu) << 8u);
    // word 21 = [nonce3, nonce4, nonce5, nonce6] but in terms of nonce
    // byte positions: word 21 starts at block-byte 84, so it holds
    // nonce[3], nonce[4], nonce[5], nonce[6].
    // n0 = nonce[0..4] LE: nonce[0] is n0's byte 0, nonce[3] is n0's byte 3.
    // n1 = nonce[4..8] LE: nonce[4] is n1's byte 0, etc.
    // We need bytes: [nonce[3], nonce[4], nonce[5], nonce[6]] as LE u32.
    w[21] = (n0 >> 24u) | ((n1 & 0x00ffffffu) << 8u);
    w[22] = (n1 >> 24u) | ((n2 & 0x00ffffffu) << 8u);
    w[23] = (n2 >> 24u) | ((n3 & 0x00ffffffu) << 8u);
    w[24] = (n3 >> 24u) | ((n4 & 0x00ffffffu) << 8u);
    w[25] = (n4 >> 24u) | ((n5 & 0x00ffffffu) << 8u);
    w[26] = (n5 >> 24u) | ((n6 & 0x00ffffffu) << 8u);
    w[27] = (n6 >> 24u) | ((n7 & 0x00ffffffu) << 8u);
    // word 28 = [nonce[31], idx_b0, idx_b1, idx_b2]
    w[28] = (n7 >> 24u) | ((call_idx & 0x00ffffffu) << 8u);
    // word 29 = [idx_b3, 0, 0, 0]
    w[29] = call_idx >> 24u;
    w[30] = 0u;
    w[31] = 0u;

    // Pack into 16 u64 message words for compress().
    var block: array<vec2<u32>, 16>;
    for (var i = 0u; i < 16u; i = i + 1u) {
        block[i] = vec2<u32>(w[i * 2u], w[i * 2u + 1u]);
    }

    // t = total bytes processed = 117 (81 input + 32 nonce + 4 index).
    // This is final block, so f = 0xFFFFFFFF.
    let t = vec2<u32>(117u, 0u);
    compress(&h, block, t, 0xffffffffu);

    // Extract 5 leaves of 12 bytes each from the first 60 bytes of h.
    // Each leaf is 3 × u32 LE.
    let words: array<u32, 16> = array<u32, 16>(
        h[0].x, h[0].y, h[1].x, h[1].y,
        h[2].x, h[2].y, h[3].x, h[3].y,
        h[4].x, h[4].y, h[5].x, h[5].y,
        h[6].x, h[6].y, h[7].x, h[7].y,
    );
    for (var k = 0u; k < 5u; k = k + 1u) {
        let leaf_id = first_leaf + k;
        if (leaf_id >= n_leaves) {
            return;
        }
        let base_out = leaf_id * 3u;
        let base_in = k * 3u;
        leaves[base_out + 0u] = words[base_in + 0u];
        leaves[base_out + 1u] = words[base_in + 1u];
        leaves[base_out + 2u] = words[base_in + 2u];
    }
}
