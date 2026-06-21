//! LZNT1 compression/decompression, mirroring `dlls/ntdll/rtl.c`.
//!
//! LZNT1 splits the input into chunks of up to 4096 bytes. Each chunk is stored
//! with a 2-byte little-endian header:
//!
//! * bits 0..=11  — `chunk_data_size - 1`
//! * bits 12..=14 — signature, always `0b011` (so a header `& 0x7000 == 0x3000`)
//! * bit 15       — set when the chunk is compressed
//!
//! A compressed chunk is a sequence of flag groups: one flag byte followed by up
//! to 8 tokens, LSB first. A `0` bit is a literal byte; a `1` bit is a 2-byte
//! back-reference whose split into (length, displacement) bits depends on the
//! current output position within the chunk — see [`displacement_bits`]. The
//! encoder here and Wine's decoder (now [`decompress_chunk`]) must agree on that
//! split exactly, which is what the golden test pins down.
//!
//! Wine's C `lznt1_compress` was a stub that only emitted *uncompressed* chunks;
//! this module is a real compressor. The decoder is a faithful, bounds-checked
//! port of Wine's `lznt1_decompress_chunk` (the part that walks attacker-supplied
//! tokens and does overlapping back-copies).

use crate::status::{STATUS_BAD_COMPRESSION_BUFFER, STATUS_BUFFER_TOO_SMALL, STATUS_SUCCESS};

/// LZNT1 chunk size (4096 bytes of uncompressed data per chunk).
const CHUNK: usize = 0x1000;

/// Upper bound on the token bytes one chunk can produce: the all-literal worst case
/// is the literals plus one flag byte per group of 8. Sizes the compressor's fixed
/// stack scratch so it never needs to allocate.
const MAX_CHUNK_OUT: usize = CHUNK + CHUNK / 8 + 1;

// Match finder tuning. Chains are confined to a single 4096-byte chunk, so a
// modest table and a bounded walk keep the worst case in check without hurting
// the ratio on typical data.
const HASH_BITS: u32 = 13;
const HASH_SIZE: usize = 1 << HASH_BITS;
const MAX_CHAIN: usize = 128;
/// Shortest encodable back-reference (the length field is biased by 3).
const MIN_MATCH: usize = 3;

/// Displacement-bit count for a back-reference emitted at output position `pos`
/// within a chunk. This mirrors the loop in Wine's `lznt1_decompress_chunk`
/// verbatim; encoder and decoder must compute it identically.
fn displacement_bits(pos: usize) -> u32 {
    let mut db = 12u32;
    while db > 4 {
        if (1usize << (db - 1)) < pos {
            break;
        }
        db -= 1;
    }
    db
}

fn hash3(b0: u8, b1: u8, b2: u8) -> usize {
    let v = (b0 as u32) | ((b1 as u32) << 8) | ((b2 as u32) << 16);
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

/// Record position `i` in the hash chains so later positions can match against it.
fn insert_pos(input: &[u8], head: &mut [i32], prev: &mut [i32], i: usize) {
    if i + 2 < input.len() {
        let h = hash3(input[i], input[i + 1], input[i + 2]);
        prev[i] = head[h];
        head[h] = i as i32;
    }
}

/// Longest back-reference for `input[pos..]`, as `(length, displacement)`, or
/// `(0, 0)` if nothing of at least [`MIN_MATCH`] bytes is available. Match
/// quality (not correctness) depends on the chain bound; any match it returns is
/// valid for the decoder.
fn find_match(input: &[u8], head: &[i32], prev: &[i32], pos: usize) -> (usize, usize) {
    let n = input.len();
    if pos == 0 || pos + 2 >= n {
        return (0, 0);
    }

    let db = displacement_bits(pos);
    let length_bits = 16 - db;
    // Max length encodable at this position, capped by the bytes that remain.
    let max_len = (((1usize << length_bits) - 1) + MIN_MATCH).min(n - pos);
    if max_len < MIN_MATCH {
        return (0, 0);
    }

    let h = hash3(input[pos], input[pos + 1], input[pos + 2]);
    let mut cand = head[h];
    let mut best_len = 0usize;
    let mut best_disp = 0usize;
    let mut chain = 0usize;

    // Every chained position is < pos, so its displacement fits in `db` bits
    // (pos <= 2^db over the whole range), i.e. no displacement clamping needed.
    while cand >= 0 && chain < MAX_CHAIN {
        let c = cand as usize;
        let mut len = 0usize;
        while len < max_len && input[c + len] == input[pos + len] {
            len += 1;
        }
        if len > best_len {
            best_len = len;
            best_disp = pos - c;
            if best_len >= max_len {
                break;
            }
        }
        cand = prev[c];
        chain += 1;
    }

    if best_len >= MIN_MATCH {
        (best_len, best_disp)
    } else {
        (0, 0)
    }
}

/// Compress one chunk (`input.len() <= CHUNK`) into `out`, returning the number of
/// token bytes written. `head`/`prev` are scratch hash chains the caller resets
/// (`head` all -1, `prev[..input.len()]` -1); `out` must hold [`MAX_CHUNK_OUT`]
/// bytes. The caller decides whether the result is smaller than storing `input` raw.
fn compress_chunk(input: &[u8], head: &mut [i32], prev: &mut [i32], out: &mut [u8]) -> usize {
    let n = input.len();
    let mut oi = 0usize;

    let mut pos = 0usize;
    while pos < n {
        let flag_index = oi;
        out[oi] = 0; // placeholder; filled in once the group is complete
        oi += 1;
        let mut flags = 0u8;

        for bit in 0..8u32 {
            if pos >= n {
                break;
            }
            let (best_len, best_disp) = find_match(input, head, prev, pos);
            if best_len >= MIN_MATCH {
                let db = displacement_bits(pos);
                let length_bits = 16 - db;
                let code = (((best_disp - 1) as u32) << length_bits) | ((best_len - MIN_MATCH) as u32);
                out[oi] = (code & 0xff) as u8;
                out[oi + 1] = (code >> 8) as u8;
                oi += 2;
                flags |= 1 << bit;
                for j in pos..pos + best_len {
                    insert_pos(input, head, prev, j);
                }
                pos += best_len;
            } else {
                out[oi] = input[pos];
                oi += 1;
                insert_pos(input, head, prev, pos);
                pos += 1;
            }
        }

        out[flag_index] = flags;
    }

    oi
}

/// Compress `src` into `dst` using LZNT1; returns the number of bytes written.
fn lznt1_compress(src: &[u8], dst: &mut [u8]) -> Result<usize, i32> {
    // Fixed scratch sized for the worst case so the compressor never allocates --
    // this lib links into a real-PE ntdll with no C runtime/heap. The ~52 KB frame
    // is stack-probed by ntdll's own __chkstk.
    let mut head = [-1i32; HASH_SIZE];
    let mut prev = [-1i32; CHUNK];
    let mut cbuf = [0u8; MAX_CHUNK_OUT];

    let mut written = 0usize;
    let mut off = 0usize;

    while off < src.len() {
        let chunk_len = (src.len() - off).min(CHUNK);
        let chunk = &src[off..off + chunk_len];

        head.fill(-1);
        prev[..chunk_len].fill(-1);
        let clen = compress_chunk(chunk, &mut head, &mut prev, &mut cbuf);

        // Only keep the compressed form if it actually saved space; otherwise the
        // chunk is stored raw (an uncompressed chunk decodes the same on any LZNT1
        // decompressor, Wine's or Windows').
        let (compressed_flag, data): (u16, &[u8]) = if clen < chunk_len {
            (0x8000, &cbuf[..clen])
        } else {
            (0x0000, chunk)
        };

        // data.len() is <= 4096, so `len - 1` always fits the 12-bit size field.
        let header = 0x3000u16 | compressed_flag | ((data.len() - 1) as u16);
        if written + 2 + data.len() > dst.len() {
            return Err(STATUS_BUFFER_TOO_SMALL);
        }
        dst[written] = (header & 0xff) as u8;
        dst[written + 1] = (header >> 8) as u8;
        written += 2;
        dst[written..written + data.len()].copy_from_slice(data);
        written += data.len();

        off += chunk_len;
    }

    Ok(written)
}

/// Decode the tokens of a single compressed LZNT1 chunk into `dst`. Returns the
/// number of bytes produced, or `None` on a malformed/truncated chunk. A faithful
/// port of Wine's `lznt1_decompress_chunk`, with all indexing bounds-checked.
fn decompress_chunk(dst: &mut [u8], src: &[u8]) -> Option<usize> {
    let mut si = 0usize;
    let mut di = 0usize;

    while si < src.len() && di < dst.len() {
        let mut flags = 0x8000u32 | src[si] as u32;
        si += 1;

        while (flags & 0xff00) != 0 && si < src.len() {
            if flags & 1 != 0 {
                // back-reference
                if si + 2 > src.len() {
                    return None;
                }
                let code = (src[si] as u32) | ((src[si + 1] as u32) << 8);
                si += 2;

                let length_bits = 16 - displacement_bits(di);
                let code_length = ((code & ((1 << length_bits) - 1)) + MIN_MATCH as u32) as usize;
                let code_displacement = ((code >> length_bits) + 1) as usize;

                if di < code_displacement {
                    return None;
                }
                // Byte-by-byte: source and destination ranges may overlap (e.g. a
                // displacement-1 run repeats a single byte).
                for _ in 0..code_length {
                    if di >= dst.len() {
                        return Some(di);
                    }
                    dst[di] = dst[di - code_displacement];
                    di += 1;
                }
            } else {
                // literal
                if di >= dst.len() {
                    return Some(di);
                }
                dst[di] = src[si];
                di += 1;
                si += 1;
            }
            flags >>= 1;
        }
    }

    Some(di)
}

/// Build a `&[u8]` from a raw pointer + length, treating null/zero as empty.
///
/// # Safety
/// If `len != 0`, `ptr` must point to `len` readable bytes for the call.
unsafe fn as_slice<'a>(ptr: *const u8, len: u32) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(ptr, len as usize) }
    }
}

/// Build a `&mut [u8]` from a raw pointer + length, treating null/zero as empty.
///
/// # Safety
/// If `len != 0`, `ptr` must point to `len` writable bytes for the call.
unsafe fn as_mut_slice<'a>(ptr: *mut u8, len: u32) -> &'a mut [u8] {
    if ptr.is_null() || len == 0 {
        &mut []
    } else {
        unsafe { core::slice::from_raw_parts_mut(ptr, len as usize) }
    }
}

/// LZNT1-compress `src` (`src_size` bytes) into `dst` (`dst_size` bytes), writing
/// the produced length to `final_size`. Backs Wine's `lznt1_compress` /
/// `RtlCompressBuffer`.
///
/// # Safety
/// `src`/`dst` must be valid for `src_size`/`dst_size` bytes (or null with size
/// 0); `final_size` must be null or a writable `u32`. ntdll guarantees this.
#[unsafe(no_mangle)]
pub extern "C" fn rs_rtl_lznt1_compress(
    src: *const u8,
    src_size: u32,
    dst: *mut u8,
    dst_size: u32,
    final_size: *mut u32,
) -> i32 {
    let src = unsafe { as_slice(src, src_size) };
    let dst = unsafe { as_mut_slice(dst, dst_size) };

    match lznt1_compress(src, dst) {
        Ok(written) => {
            if !final_size.is_null() {
                unsafe { *final_size = written as u32 };
            }
            STATUS_SUCCESS
        }
        Err(status) => status,
    }
}

/// Decode one compressed LZNT1 chunk (`src`) into `dst`, writing the produced
/// length to `written`. Returns `STATUS_SUCCESS` or `STATUS_BAD_COMPRESSION_BUFFER`.
/// Backs Wine's `lznt1_decompress_chunk`.
///
/// # Safety
/// `dst`/`src` must be valid for `dst_size`/`src_size` bytes (or null with size
/// 0); `written` must be null or a writable `u32`. ntdll guarantees this.
#[unsafe(no_mangle)]
pub extern "C" fn rs_rtl_lznt1_decompress_chunk(
    dst: *mut u8,
    dst_size: u32,
    src: *const u8,
    src_size: u32,
    written: *mut u32,
) -> i32 {
    let dst = unsafe { as_mut_slice(dst, dst_size) };
    let src = unsafe { as_slice(src, src_size) };

    match decompress_chunk(dst, src) {
        Some(count) => {
            if !written.is_null() {
                unsafe { *written = count as u32 };
            }
            STATUS_SUCCESS
        }
        None => STATUS_BAD_COMPRESSION_BUFFER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk an LZNT1 stream (chunk headers + payloads) back to plaintext. Mirrors
    /// the offset-0 path of Wine's `lznt1_decompress`; sufficient for round trips
    /// since every non-final chunk this compressor emits is exactly 4096 bytes.
    fn decompress_all(src: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut si = 0usize;
        while si + 2 <= src.len() {
            let header = (src[si] as u16) | ((src[si + 1] as u16) << 8);
            si += 2;
            if header == 0 {
                break;
            }
            assert_eq!(header & 0x7000, 0x3000, "bad chunk signature {header:#06x}");
            let chunk_size = ((header & 0xfff) as usize) + 1;
            if si + chunk_size > src.len() {
                return None;
            }
            let chunk = &src[si..si + chunk_size];
            si += chunk_size;

            if header & 0x8000 != 0 {
                let mut buf = vec![0u8; CHUNK];
                let w = decompress_chunk(&mut buf, chunk)?;
                out.extend_from_slice(&buf[..w]);
            } else {
                out.extend_from_slice(chunk);
            }
        }
        Some(out)
    }

    fn roundtrip(input: &[u8]) {
        let mut dst = vec![0u8; input.len() * 2 + 64];
        let written = lznt1_compress(input, &mut dst).expect("compress");
        let decoded = decompress_all(&dst[..written]).expect("decompress");
        assert_eq!(decoded, input, "round trip mismatch (len {})", input.len());
    }

    #[test]
    fn decoder_matches_windows_format() {
        // Golden vector from Wine's tests/rtl.c: a compressed chunk payload that
        // decodes to "WineWine" (literals W,i,n,e then a len-4/disp-4 back-ref).
        let chunk = [0x10u8, b'W', b'i', b'n', b'e', 0x01, 0x30];
        let mut dst = [0u8; 16];
        let n = decompress_chunk(&mut dst, &chunk).unwrap();
        assert_eq!(&dst[..n], b"WineWine");
    }

    #[test]
    fn displacement_bits_boundaries() {
        assert_eq!(displacement_bits(1), 4);
        assert_eq!(displacement_bits(16), 4);
        assert_eq!(displacement_bits(17), 5);
        assert_eq!(displacement_bits(32), 5);
        assert_eq!(displacement_bits(2048), 11);
        assert_eq!(displacement_bits(2049), 12);
        assert_eq!(displacement_bits(4095), 12);
    }

    #[test]
    fn roundtrips_various_inputs() {
        roundtrip(b"");
        roundtrip(b"W");
        roundtrip(b"WineWineWine\0");
        roundtrip(&[0u8; 10_000]); // highly compressible (RLE via disp-1)
        roundtrip(&vec![0x11u8; CHUNK]); // exactly one chunk
        roundtrip(&vec![0x42u8; CHUNK * 2 + 5]); // multi-chunk + remainder

        // A repetitive pattern that exercises real back-references.
        let pattern: Vec<u8> = (0..5000).map(|i| b"SpritzWine"[i % 10]).collect();
        roundtrip(&pattern);

        // Pseudo-incompressible (deterministic LCG): forces uncompressed-chunk fallback.
        let mut x = 0x1234_5678u32;
        let noise: Vec<u8> = (0..9000)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 24) as u8
            })
            .collect();
        roundtrip(&noise);
    }

    #[test]
    fn compresses_repetitive_data() {
        let input = vec![0xABu8; 4096];
        let mut dst = vec![0u8; input.len() * 2 + 64];
        let written = lznt1_compress(&input, &mut dst).unwrap();
        assert!(written < input.len(), "expected compression, got {written} bytes");
    }

    #[test]
    fn wine_test_buffer_compresses_below_input() {
        // Wine's tests/rtl.c asserts final_size < sizeof("WineWineWine") (13).
        let input = b"WineWineWine\0";
        let mut dst = vec![0u8; 64];
        let written = lznt1_compress(input, &mut dst).unwrap();
        assert!(written < input.len(), "final_size {written} not < {}", input.len());
        let header = (dst[0] as u16) | ((dst[1] as u16) << 8);
        assert_eq!(header & 0x7000, 0x3000, "chunk signature {header:#06x}");
        // And it must decode back to the original.
        assert_eq!(decompress_all(&dst[..written]).unwrap(), input);
    }

    #[test]
    fn compress_reports_buffer_too_small() {
        let input = b"WineWineWine\0";
        let mut tiny = [0u8; 4];
        assert_eq!(lznt1_compress(input, &mut tiny), Err(STATUS_BUFFER_TOO_SMALL));
    }

    #[test]
    fn decompress_rejects_truncated_backref() {
        // Flag says "back-reference" but only one of the two code bytes is present.
        let chunk = [0x01u8, 0x00];
        let mut dst = [0u8; 16];
        assert_eq!(decompress_chunk(&mut dst, &chunk), None);
    }
}
