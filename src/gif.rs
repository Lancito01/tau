//! Redacts a GIF by rewriting palette indexes inside the file, so nothing outside the boxes is re-quantized.
//! Palettes, timing, loop count and every frame that misses the boxes stay byte for byte.
//! Pure logic with no Win32, covered by `tests/fidelity.rs`.

pub fn screen_size(b: &[u8]) -> Option<(u32, u32)> {
    if !matches!(b.get(..6)?, b"GIF87a" | b"GIF89a") { return None; }
    Some((u16::from_le_bytes([*b.get(6)?, *b.get(7)?]) as u32, u16::from_le_bytes([*b.get(8)?, *b.get(9)?]) as u32))
}

/// Concatenates sub-blocks starting at `i`. Returns the data and the offset just past the terminator.
fn read_blocks(b: &[u8], mut i: usize) -> Result<(Vec<u8>, usize), String> {
    let mut data = vec![];
    loop {
        let n = *b.get(i).ok_or("GIF ends early")? as usize;
        i += 1;
        if n == 0 { return Ok((data, i)); }
        data.extend_from_slice(b.get(i..i + n).ok_or("GIF ends early")?);
        i += n;
    }
}

fn write_blocks(out: &mut Vec<u8>, data: &[u8]) {
    for c in data.chunks(255) {
        out.push(c.len() as u8);
        out.extend_from_slice(c);
    }
    out.push(0);
}

/// LZW decode into `pixels` indexes. A truncated stream is padded with index 0, as decoders display it.
pub fn lzw_decode(data: &[u8], min_code: u8, pixels: usize) -> Result<Vec<u8>, String> {
    if !(1..=11).contains(&min_code) { return Err("Bad GIF code size".into()); }
    let clear = 1usize << min_code;
    let end = clear + 1;
    let mut out = Vec::with_capacity(pixels);
    // Each entry is (previous code, last byte, length). Entries below `clear` are single bytes.
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut length = vec![0u16; 4096];
    for i in 0..clear { suffix[i] = i as u8; length[i] = 1; }
    let mut next = end + 1;
    let mut width = min_code as u32 + 1;
    let mut prev: Option<usize> = None;
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut scratch = vec![];
    'bytes: for &byte in data {
        acc |= (byte as u32) << bits;
        bits += 8;
        while bits >= width {
            let code = (acc & ((1 << width) - 1)) as usize;
            acc >>= width;
            bits -= width;
            if code == clear {
                next = end + 1;
                width = min_code as u32 + 1;
                prev = None;
                continue;
            }
            if code == end { break 'bytes; }
            scratch.clear();
            let first;
            if code < next && (code < clear || code > end) {
                let mut c = code;
                for _ in 0..length[code] { scratch.push(suffix[c]); c = prefix[c] as usize; }
                first = *scratch.last().unwrap();
            } else if code == next && prev.is_some() {
                let p = prev.unwrap();
                let mut c = p;
                for _ in 0..length[p] { scratch.push(suffix[c]); c = prefix[c] as usize; }
                first = *scratch.last().unwrap();
                scratch.insert(0, first);
            } else {
                return Err("Corrupt GIF image data".into());
            }
            out.extend(scratch.iter().rev());
            if let Some(p) = prev {
                if next < 4096 {
                    prefix[next] = p as u16;
                    suffix[next] = first;
                    length[next] = length[p] + 1;
                    next += 1;
                    if next == (1 << width) && width < 12 { width += 1; }
                }
            }
            prev = Some(code);
            if out.len() >= pixels { break 'bytes; }
        }
    }
    out.resize(pixels, 0);
    Ok(out)
}

pub fn lzw_encode(px: &[u8], min_code: u8) -> Vec<u8> {
    let clear = 1u32 << min_code;
    let end = clear + 1;
    let mut out = vec![];
    let (mut acc, mut bits) = (0u64, 0u32);
    let mut width = min_code as u32 + 1;
    let mut emit = |code: u32, width: u32, out: &mut Vec<u8>| {
        acc |= (code as u64) << bits;
        bits += width;
        while bits >= 8 { out.push(acc as u8); acc >>= 8; bits -= 8; }
    };
    let mut table: std::collections::HashMap<(u32, u8), u32> = std::collections::HashMap::new();
    let mut next = end + 1;
    emit(clear, width, &mut out);
    let mut cur: Option<u32> = None;
    for &p in px {
        let Some(c) = cur else { cur = Some(p as u32); continue };
        if let Some(&n) = table.get(&(c, p)) { cur = Some(n); continue; }
        emit(c, width, &mut out);
        if next < 4096 {
            table.insert((c, p), next);
            // The decoder adds this entry one code later, so it widens when `next` passes the limit.
            if next == (1 << width) && width < 12 { width += 1; }
            next += 1;
        } else {
            emit(clear, width, &mut out);
            table.clear();
            next = end + 1;
            width = min_code as u32 + 1;
        }
        cur = Some(p as u32);
    }
    if let Some(c) = cur { emit(c, width, &mut out); }
    // The decoder has one more entry by now, which may have widened the end code.
    if cur.is_some() && next < 4096 && next == (1 << width) && width < 12 { width += 1; }
    emit(end, width, &mut out);
    emit(0, 7, &mut out);
    out
}

/// Row order of an interlaced frame: the row in the picture for each row in the file.
fn row_order(h: usize, interlaced: bool) -> Vec<usize> {
    if !interlaced { return (0..h).collect(); }
    [(0, 8), (4, 8), (2, 4), (1, 2)].iter().flat_map(|&(s, d)| (s..h).step_by(d)).collect()
}

/// The darkest colour in a palette, so the box reads as black even without black in the palette.
fn darkest(table: &[u8], transparent: Option<u8>) -> Option<u8> {
    table.chunks_exact(3).enumerate()
        .filter(|(i, _)| Some(*i as u8) != transparent)
        .min_by_key(|(_, c)| 299 * c[0] as u32 + 587 * c[1] as u32 + 114 * c[2] as u32)
        .map(|(i, _)| i as u8)
}

/// Paints the boxes, given in screen pixels, in every frame. Frames that miss them are copied byte for byte.
/// Transparent pixels inside a box are painted too, since they would otherwise show an earlier frame through.
pub fn redact(b: &[u8], boxes: &[(u32, u32, u32, u32)]) -> Result<Vec<u8>, String> {
    screen_size(b).ok_or("Not a GIF")?;
    let flags = *b.get(10).ok_or("GIF ends early")?;
    let mut i = 13;
    let global: &[u8] = if flags & 0x80 != 0 {
        let n = 3usize << ((flags & 7) + 1);
        let t = b.get(i..i + n).ok_or("GIF ends early")?;
        i += n;
        t
    } else { &[] };
    let mut out = b[..i].to_vec();
    let mut transparent = None;
    loop {
        match *b.get(i).ok_or("GIF ends early")? {
            0x3B => { out.push(0x3B); return Ok(out); }
            0x21 => {
                let label = *b.get(i + 1).ok_or("GIF ends early")?;
                let (data, end) = read_blocks(b, i + 2)?;
                if label == 0xF9 && data.len() >= 4 { transparent = (data[0] & 1 != 0).then_some(data[3]); }
                out.extend_from_slice(&b[i..end]);
                i = end;
            }
            0x2C => {
                let d = b.get(i..i + 10).ok_or("GIF ends early")?;
                let n16 = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as usize;
                let (fx, fy, fw, fh) = (n16(1), n16(3), n16(5), n16(7));
                let f = d[9];
                let mut j = i + 10;
                let table = if f & 0x80 != 0 {
                    let n = 3usize << ((f & 7) + 1);
                    let t = b.get(j..j + n).ok_or("GIF ends early")?;
                    j += n;
                    t
                } else { global };
                let min_code = *b.get(j).ok_or("GIF ends early")?;
                let (data, end) = read_blocks(b, j + 1)?;
                let hits = |&&(x, y, w, h): &&(u32, u32, u32, u32)| (x as usize) < fx + fw && fx < (x + w) as usize && (y as usize) < fy + fh && fy < (y + h) as usize;
                if !boxes.iter().any(|bx| hits(&bx)) {
                    out.extend_from_slice(&b[i..end]);
                } else {
                    let ink = darkest(table, transparent).ok_or("This GIF frame has no colour table")?;
                    let mut px = lzw_decode(&data, min_code, fw * fh)?;
                    let rows = row_order(fh, f & 0x40 != 0);
                    for (file_row, &row) in rows.iter().enumerate() {
                        let sy = fy + row;
                        for &(x, y, w, h) in boxes {
                            if sy < y as usize || sy >= (y + h) as usize { continue; }
                            let from = (x as usize).max(fx);
                            let to = ((x + w) as usize).min(fx + fw);
                            for sx in from..to { px[file_row * fw + sx - fx] = ink; }
                        }
                    }
                    out.extend_from_slice(&b[i..=j]);
                    write_blocks(&mut out, &lzw_encode(&px, min_code));
                }
                i = end;
                transparent = None;
            }
            // Some writers leave padding between blocks, and some stop without a trailer.
            0x00 => { i += 1; }
            _ => { out.push(0x3B); return Ok(out); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lzw_round_trips_every_pattern() {
        let mut noise = vec![];
        let mut s = 12345u32;
        for _ in 0..200_000 { s = s.wrapping_mul(1664525).wrapping_add(1013904223); noise.push((s >> 24) as u8); }
        let cases: Vec<(Vec<u8>, u8)> = vec![
            (vec![0; 100_000], 8), (noise.clone(), 8),
            (noise.iter().map(|n| n & 1).collect(), 2),
            (noise.iter().map(|n| n & 15).collect(), 4),
            ((0..100_000u32).map(|i| (i % 7) as u8).collect(), 3),
            (vec![3], 2), (vec![1, 1], 2),
        ];
        for (px, code) in cases {
            let enc = lzw_encode(&px, code);
            assert_eq!(lzw_decode(&enc, code, px.len()).unwrap(), px, "code size {code}, {} pixels", px.len());
        }
    }
    #[test]
    fn interlaced_rows_visit_each_row_once() {
        let mut r = row_order(19, true);
        assert_eq!(&r[..3], &[0, 8, 16]);
        r.sort();
        assert_eq!(r, (0..19).collect::<Vec<_>>());
    }
    #[test]
    fn darkest_skips_the_transparent_entry() {
        let t = [0, 0, 0, 200, 200, 200, 20, 20, 20];
        assert_eq!(darkest(&t, None), Some(0));
        assert_eq!(darkest(&t, Some(0)), Some(2));
    }
    #[test]
    fn garbage_is_refused() {
        assert!(screen_size(b"nope").is_none());
        assert!(redact(b"nope", &[]).is_err());
        assert!(redact(b"GIF89a\x01\0\x01\0", &[]).is_err());
    }
}
