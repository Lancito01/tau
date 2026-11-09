//! Redacts a GIF by rewriting palette indexes inside the file, so nothing outside the marks is re-quantized.
//! Palettes, timing, loop count and every frame that misses the marks stay byte for byte.
//! Pure logic with no Win32, covered by `tests/fidelity.rs`.
//! Only hard-edged solid marks can work this way. A blur or feather needs colours the palette may not have.
use crate::redact::{Fill, Mark};

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

/// The closest palette entry to a colour, leaving out the transparent one.
fn nearest(table: &[u8], transparent: Option<u8>, rgb: [u8; 3]) -> Option<u8> {
    if rgb == [0, 0, 0] { return darkest(table, transparent); }
    table.chunks_exact(3).enumerate()
        .filter(|(i, _)| Some(*i as u8) != transparent)
        .min_by_key(|(_, c)| (0..3).map(|k| (c[k] as i32 - rgb[k] as i32).pow(2)).sum::<i32>())
        .map(|(i, _)| i as u8)
}

/// Paints solid marks, given in screen pixels, in every frame. Frames that miss them are copied byte for byte. Each frame
/// uses its palette's closest colour. Transparent pixels inside a mark are painted too, since they would otherwise show
/// an earlier frame through.
pub fn redact(b: &[u8], marks: &[Mark]) -> Result<Vec<u8>, String> {
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
                let hits = |m: &&Mark| (m.x as usize) < fx + fw && fx < (m.x + m.w) as usize && (m.y as usize) < fy + fh && fy < (m.y + m.h) as usize;
                if !marks.iter().any(|m| hits(&m)) {
                    out.extend_from_slice(&b[i..end]);
                } else {
                    let mut px = lzw_decode(&data, min_code, fw * fh)?;
                    let rows = row_order(fh, f & 0x40 != 0);
                    for m in marks {
                        let Fill::Solid(rgb) = m.fill else { return Err("A GIF blur cannot be painted in place".into()) };
                        let ink = nearest(table, transparent, rgb).ok_or("This GIF frame has no colour table")?;
                        for (file_row, &row) in rows.iter().enumerate() {
                            let sy = (fy + row) as u32;
                            if sy < m.y || sy >= m.y + m.h { continue; }
                            let from = m.x.max(fx as u32);
                            let to = (m.x + m.w).min((fx + fw) as u32);
                            for sx in from..to {
                                if m.coverage(sx, sy) > 0.0 { px[file_row * fw + sx as usize - fx] = ink; }
                            }
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

/// A GIF split into its header and its frames, each frame with the extensions in front of it.
struct Parsed<'a> {
    /// Signature, screen descriptor and global colour table.
    head: &'a [u8],
    global: &'a [u8],
    frames: Vec<Frame<'a>>,
}

struct Frame<'a> {
    /// Extensions before the image, each whole, with its label.
    exts: Vec<(u8, &'a [u8])>,
    /// Image descriptor, then the local colour table if there is one.
    desc: &'a [u8],
    /// Code size byte and the image data's sub-blocks, terminator included.
    data: &'a [u8],
}

fn parse(b: &[u8]) -> Result<Parsed<'_>, String> {
    screen_size(b).ok_or("Not a GIF")?;
    let flags = *b.get(10).ok_or("GIF ends early")?;
    let mut i = 13;
    let gstart = i;
    if flags & 0x80 != 0 { i += 3usize << ((flags & 7) + 1); }
    let global = b.get(gstart..i).ok_or("GIF ends early")?;
    let head = &b[..i];
    let (mut frames, mut exts) = (vec![], vec![]);
    loop {
        match b.get(i) {
            None | Some(0x3B) => break,
            Some(0x21) => {
                let label = *b.get(i + 1).ok_or("GIF ends early")?;
                let (_, end) = read_blocks(b, i + 2)?;
                exts.push((label, &b[i..end]));
                i = end;
            }
            Some(0x2C) => {
                let d = b.get(i..i + 10).ok_or("GIF ends early")?;
                let mut j = i + 10;
                if d[9] & 0x80 != 0 { j += 3usize << ((d[9] & 7) + 1); }
                let desc = b.get(i..j).ok_or("GIF ends early")?;
                let (_, end) = read_blocks(b, j + 1)?;
                frames.push(Frame { exts: std::mem::take(&mut exts), desc, data: &b[j..end] });
                i = end;
            }
            Some(0x00) => i += 1,
            Some(_) => break,
        }
    }
    Ok(Parsed { head, global, frames })
}

/// Joins GIFs that are consecutive parts of one encode, as `quality::gif_part_args` makes them, into what a single pass
/// would show: the same pictures with the same delays. Every part but the last ends with one frame too many, the first
/// of the next part, and that frame is dropped. The header and loop count come from the first part. A frame that used its
/// own part's global colour table gets that table as a local one, since the first part's global table may differ.
pub fn splice(parts: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let parsed = parts.iter().map(|p| parse(p)).collect::<Result<Vec<_>, _>>()?;
    let first = parsed.first().ok_or("Nothing to join")?;
    let mut out = first.head.to_vec();
    let last = parsed.len() - 1;
    for (k, p) in parsed.iter().enumerate() {
        let keep = if k < last { p.frames.len().checked_sub(1).ok_or("A part of the GIF came out empty")? } else { p.frames.len() };
        let same_global = p.global == first.global;
        let gflags = p.head[10];
        for (n, f) in p.frames[..keep].iter().enumerate() {
            for (label, e) in &f.exts {
                // The loop count and anything else ahead of a later part's first picture belong to that file, not the frame.
                if k > 0 && n == 0 && *label != 0xF9 { continue; }
                out.extend_from_slice(e);
            }
            let has_local = f.desc[9] & 0x80 != 0;
            if k == 0 || has_local || same_global {
                out.extend_from_slice(f.desc);
            } else {
                if p.global.is_empty() { return Err("A GIF frame has no colour table".into()); }
                out.extend_from_slice(&f.desc[..9]);
                // Keep the interlace bit, and carry the table's size and sort flag over from the screen descriptor.
                out.push(0x80 | (f.desc[9] & 0x40) | if gflags & 0x08 != 0 { 0x20 } else { 0 } | (gflags & 7));
                out.extend_from_slice(p.global);
            }
            out.extend_from_slice(f.data);
        }
    }
    out.push(0x3B);
    Ok(out)
}

/// The number of pictures in a GIF.
pub fn frame_count(b: &[u8]) -> Result<usize, String> { Ok(parse(b)?.frames.len()) }

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-pixel-high GIF with a global table of `g` and one frame per entry of `frames`: (local table, delay, index).
    fn make(g: &[u8], frames: &[(Option<&[u8]>, u8, u8)], loop_ext: bool) -> Vec<u8> {
        let mut b = b"GIF89a\x01\x00\x01\x00".to_vec();
        b.extend_from_slice(&[0xF0, 0, 0]);
        b.extend_from_slice(g);
        if loop_ext { b.extend_from_slice(b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00"); }
        for (local, delay, px) in frames {
            b.extend_from_slice(&[0x21, 0xF9, 4, 0, *delay, 0, 0, 0]);
            b.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0]);
            match local { Some(t) => { b.push(0x80); b.extend_from_slice(t); } None => b.push(0) }
            b.push(2);
            write_blocks(&mut b, &lzw_encode(&[*px], 2));
        }
        b.push(0x3B);
        b
    }

    #[test]
    fn splice_drops_the_overlap_and_keeps_each_frames_colours() {
        let (ga, gb) = ([0u8, 0, 0, 255, 255, 255], [9u8, 9, 9, 7, 7, 7]);
        let a = make(&ga, &[(None, 3, 0), (None, 4, 1), (Some(&[1, 2, 3, 4, 5, 6]), 9, 1)], true);
        let b = make(&gb, &[(Some(&[1, 2, 3, 4, 5, 6]), 3, 1), (None, 3, 0)], true);
        let s = splice(&[a.clone(), b]).unwrap();
        let p = parse(&s).unwrap();
        assert_eq!(p.head, parse(&a).unwrap().head);
        assert_eq!(p.frames.len(), 4, "the first part's extra frame is dropped");
        assert_eq!(p.frames.iter().map(|f| f.exts.iter().find(|e| e.0 == 0xF9).unwrap().1[4]).collect::<Vec<_>>(), vec![3, 4, 3, 3]);
        assert_eq!(p.frames.iter().map(|f| f.exts.iter().filter(|e| e.0 == 0xFF).count()).sum::<usize>(), 1, "one loop count");
        assert_eq!(p.frames[2].desc[10..], [1, 2, 3, 4, 5, 6]);
        assert_eq!(p.frames[3].desc[9] & 0x87, 0x80, "the second part's global table travels with its frame");
        assert_eq!(&p.frames[3].desc[10..], &gb);
        assert_eq!(p.frames[3].data, parse(&make(&gb, &[(None, 3, 0)], false)).unwrap().frames[0].data);
    }

    #[test]
    fn splice_of_one_part_is_that_part() {
        let a = make(&[0, 0, 0, 255, 255, 255], &[(None, 3, 0), (None, 4, 1)], true);
        assert_eq!(splice(&[a.clone()]).unwrap(), a);
    }

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
    fn nearest_finds_the_closest_colour() {
        let t = [0, 0, 0, 250, 10, 10, 10, 10, 250];
        assert_eq!(nearest(&t, None, [255, 0, 0]), Some(1));
        assert_eq!(nearest(&t, Some(1), [255, 0, 0]), Some(0));
        assert_eq!(nearest(&t, None, [0, 0, 240]), Some(2));
    }
    #[test]
    fn garbage_is_refused() {
        assert!(screen_size(b"nope").is_none());
        assert!(redact(b"nope", &[]).is_err());
        assert!(redact(b"GIF89a\x01\0\x01\0", &[]).is_err());
    }
}
