//! Writes HEIC files without the Windows codecs. ffmpeg's x265 makes the picture, but ffmpeg has no way to store one as
//! an image, so the container is built here: a HEIC is an HEVC picture and a few boxes describing it.

/// x265 refuses very small pictures, so these are stored larger and cropped back.
const MIN_SIDE: u32 = 16;

/// A picture's size on screen and as stored. HEVC 4:2:0 needs even sides.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size { pub w: u32, pub h: u32, pub coded_w: u32, pub coded_h: u32 }

pub fn size_for(w: u32, h: u32) -> Size {
    Size { w, h, coded_w: (w + (w & 1)).max(MIN_SIDE), coded_h: (h + (h & 1)).max(MIN_SIDE) }
}

/// Grows the frame to the stored size, repeating the edge so the crop line hides no dark fringe.
fn fit_filter(s: Size) -> String {
    if (s.w, s.h) == (s.coded_w, s.coded_h) { return String::new(); }
    let (right, bottom) = (s.coded_w - s.w, s.coded_h - s.h);
    // Only a thin border can be smeared, and only a large picture has one worth the trouble.
    if right <= 1 && bottom <= 1 { return format!("pad={}:{},fillborders=right={right}:bottom={bottom}:mode=smear,", s.coded_w, s.coded_h); }
    format!("pad={}:{},", s.coded_w, s.coded_h)
}

fn x265(crf: u32, params: &str, out: &str) -> Vec<String> {
    let crf = crf.to_string();
    ["-frames:v", "1", "-an", "-c:v", "libx265", "-preset", "slow", "-crf", &crf, "-x265-params", params, "-f", "hevc", out].iter().map(|s| s.to_string()).collect()
}

/// ffmpeg arguments, after the input, for the picture as a bare HEVC stream. 4:2:0 at 8 bits is the one kind every HEIC
/// viewer decodes.
pub fn picture_args(s: Size, crf: u32, out: &str) -> Vec<String> {
    let vf = format!("format=gbrp,{}scale=out_color_matrix=bt709:out_range=tv:flags=lanczos+accurate_rnd+full_chroma_int,format=yuv420p", fit_filter(s));
    let mut a = vec!["-vf".to_string(), vf];
    a.extend(x265(crf, "log-level=error:info=0:colorprim=bt709:transfer=iec61966-2-1:colormatrix=bt709:range=limited", out));
    a
}

/// The same for transparency, which HEIC stores as a second, grey picture.
pub fn alpha_args(s: Size, crf: u32, out: &str) -> Vec<String> {
    let vf = format!("format=rgba,alphaextract,format=gray,{}scale=in_range=pc:out_range=pc,format=yuvj420p", fit_filter(s));
    let mut a = vec!["-vf".to_string(), vf];
    a.extend(x265(crf, "log-level=error:info=0:range=full", out));
    a
}

/// A stream split into what the container wants: its headers apart, and its picture with lengths for start codes.
pub struct Stream { vps: Vec<u8>, sps: Vec<u8>, pps: Vec<u8>, data: Vec<u8> }

/// Splits an Annex B stream on its start codes.
fn nal_units(b: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i..i + 3] == [0, 0, 1] { starts.push(i + 3); i += 3; } else { i += 1; }
    }
    starts.iter().enumerate().map(|(n, &s)| {
        let mut e = starts.get(n + 1).map(|&x| x - 3).unwrap_or(b.len());
        // Zeros before a start code belong to neither unit.
        while e > s && b[e - 1] == 0 { e -= 1; }
        &b[s..e]
    }).collect()
}

pub fn parse_stream(b: &[u8]) -> Result<Stream, String> {
    let mut st = Stream { vps: vec![], sps: vec![], pps: vec![], data: vec![] };
    for n in nal_units(b) {
        let Some(&head) = n.first() else { continue };
        match (head >> 1) & 0x3F {
            32 => st.vps = n.to_vec(),
            33 => st.sps = n.to_vec(),
            34 => st.pps = n.to_vec(),
            t if t < 32 => {
                st.data.extend((n.len() as u32).to_be_bytes());
                st.data.extend_from_slice(n);
            }
            _ => {}
        }
    }
    if st.vps.is_empty() || st.sps.is_empty() || st.pps.is_empty() || st.data.is_empty() { return Err("The encoder did not produce a complete picture".into()); }
    Ok(st)
}

/// The profile, tier and level fields, 12 bytes straight after the SPS's first byte. Escapes have to come out first.
fn profile_of(sps: &[u8]) -> Result<[u8; 12], String> {
    let mut raw = Vec::with_capacity(32);
    let mut zeros = 0;
    for &x in sps.iter().skip(2).take(24) {
        if zeros >= 2 && x == 3 { zeros = 0; continue; }
        zeros = if x == 0 { zeros + 1 } else { 0 };
        raw.push(x);
    }
    raw.get(1..13).and_then(|s| s.try_into().ok()).ok_or_else(|| "The encoder wrote an unreadable header".to_string())
}

// ---- boxes ----

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

/// A box that starts with a version and flags. Only version matters here.
fn full(kind: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![version, 0, 0, 0];
    v.extend_from_slice(body);
    boxed(kind, &v)
}

fn hvcc(st: &Stream) -> Result<Vec<u8>, String> {
    let p = profile_of(&st.sps)?;
    let mut b = vec![1];
    b.extend_from_slice(&p);
    b.extend([0xF0, 0x00, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0]); // no segmentation or parallelism, 4:2:0, 8 bits, no rate
    b.push(0x0F); // one temporal layer, nested, lengths of 4 bytes
    b.push(3);
    for (t, n) in [(32u8, &st.vps), (33, &st.sps), (34, &st.pps)] {
        b.push(0x80 | t);
        b.extend(1u16.to_be_bytes());
        b.extend((n.len() as u16).to_be_bytes());
        b.extend_from_slice(n);
    }
    Ok(boxed(b"hvcC", &b))
}

fn ispe(s: Size) -> Vec<u8> {
    let mut b = s.coded_w.to_be_bytes().to_vec();
    b.extend(s.coded_h.to_be_bytes());
    full(b"ispe", 0, &b)
}

/// Crops what was padded on the right and at the bottom. Offsets count from the centre, hence the halves.
fn clap(s: Size) -> Vec<u8> {
    let mut b = vec![];
    let off = |shown: u32, coded: u32| shown as i32 - coded as i32;
    for (n, d) in [(s.w as i32, 1), (s.h as i32, 1), (off(s.w, s.coded_w), 2), (off(s.h, s.coded_h), 2)] {
        b.extend(n.to_be_bytes());
        b.extend((d as u32).to_be_bytes());
    }
    boxed(b"clap", &b)
}

fn colr_nclx(full_range: bool) -> Vec<u8> {
    let mut b = b"nclx".to_vec();
    b.extend([0, 1, 0, 13, 0, 1, if full_range { 0x80 } else { 0 }]); // BT.709 primaries, sRGB transfer, BT.709 matrix
    boxed(b"colr", &b)
}

fn infe(id: u16) -> Vec<u8> {
    let mut b = id.to_be_bytes().to_vec();
    b.extend([0, 0]);
    b.extend_from_slice(b"hvc1");
    b.push(0);
    full(b"infe", 2, &b)
}

/// Lists one item's properties: 1-based positions, with the top bit where a reader must understand it to show the item.
fn ipma_entry(id: u16, props: &[(u8, bool)]) -> Vec<u8> {
    let mut b = id.to_be_bytes().to_vec();
    b.push(props.len() as u8);
    for (i, essential) in props { b.push(i | if *essential { 0x80 } else { 0 }); }
    b
}

/// A complete file. `alpha`, when given, is the transparency for the same size. `icc` is the colour profile, kept beside
/// the colour description the picture itself needs.
pub fn mux(picture: &Stream, alpha: Option<&Stream>, s: Size, icc: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let mut ftyp = b"heic".to_vec();
    ftyp.extend([0, 0, 0, 0]);
    ftyp.extend_from_slice(b"mif1heic");
    let ftyp = boxed(b"ftyp", &ftyp);

    let cropped = (s.w, s.h) != (s.coded_w, s.coded_h);
    let mut ipco = vec![];
    let mut push = |b: Vec<u8>| { ipco.extend(b); };
    push(hvcc(picture)?); // 1
    push(ispe(s)); // 2
    push(colr_nclx(false)); // 3
    let mut next = 4u8;
    let mut main: Vec<(u8, bool)> = vec![(1, true), (2, false), (3, false)];
    if let Some(p) = icc {
        let mut b = b"prof".to_vec();
        b.extend_from_slice(p);
        push(boxed(b"colr", &b));
        main.push((next, false));
        next += 1;
    }
    let crop_index = next;
    if cropped {
        push(clap(s));
        main.push((crop_index, true));
        next += 1;
    }
    let mut entries = ipma_entry(1, &main);
    if let Some(a) = alpha {
        push(hvcc(a)?);
        let mut urn = b"urn:mpeg:hevc:2015:auxid:1".to_vec();
        urn.push(0);
        push(full(b"auxC", 0, &urn));
        let mut props = vec![(next, true), (2, false), (next + 1, false)];
        if cropped { props.push((crop_index, true)); }
        entries.extend(ipma_entry(2, &props));
    }
    let items: u32 = if alpha.is_some() { 2 } else { 1 };
    let mut ipma = items.to_be_bytes().to_vec();
    ipma.extend(entries);
    let iprp = boxed(b"iprp", &[boxed(b"ipco", &ipco), full(b"ipma", 0, &ipma)].concat());

    let mut hdlr = vec![0; 4];
    hdlr.extend_from_slice(b"pict");
    hdlr.extend([0; 13]);
    let mut iinf = (items as u16).to_be_bytes().to_vec();
    iinf.extend(infe(1));
    if alpha.is_some() { iinf.extend(infe(2)); }
    let iref = alpha.map(|_| {
        let mut b = 2u16.to_be_bytes().to_vec();
        b.extend(1u16.to_be_bytes());
        b.extend(1u16.to_be_bytes());
        full(b"iref", 0, &boxed(b"auxl", &b))
    }).unwrap_or_default();

    // The item table holds file offsets, and its own size is the same whatever they are: build once to measure.
    let meta = |data_at: u32| {
        let mut iloc = vec![0x44, 0x00];
        iloc.extend((items as u16).to_be_bytes());
        let mut at = data_at;
        let mut item = |id: u16, len: usize| {
            iloc.extend(id.to_be_bytes());
            iloc.extend([0, 0, 0, 1]);
            iloc.extend(at.to_be_bytes());
            iloc.extend((len as u32).to_be_bytes());
            at += len as u32;
        };
        item(1, picture.data.len());
        if let Some(a) = alpha { item(2, a.data.len()); }
        let body = [full(b"hdlr", 0, &hdlr), full(b"pitm", 0, &1u16.to_be_bytes()), full(b"iloc", 0, &iloc), full(b"iinf", 0, &iinf), iref.clone(), iprp.clone()].concat();
        full(b"meta", 0, &body)
    };
    let data_at = (ftyp.len() + meta(0).len() + 8) as u32;
    let mut mdat = picture.data.clone();
    if let Some(a) = alpha { mdat.extend_from_slice(&a.data); }
    Ok([ftyp, meta(data_at), boxed(b"mdat", &mdat)].concat())
}

#[cfg(test)]
mod heif_tests {
    use super::*;

    fn stream() -> Vec<u8> {
        let mut b = vec![0, 0, 0, 1, 32 << 1, 1, 9, 9];
        // An SPS whose profile bytes contain a run that had to be escaped.
        b.extend([0, 0, 1, 33 << 1, 1, 1, 1, 0x60, 0, 0, 3, 0, 0x90, 0, 0, 3, 0, 0, 3, 0, 0, 3, 0, 0x5D, 7]);
        b.extend([0, 0, 1, 34 << 1, 1, 5]);
        b.extend([0, 0, 1, 39 << 1, 1, 7]); // SEI, which the container has no use for
        b.extend([0, 0, 1, 19 << 1, 1, 0xAA, 0xBB]);
        b
    }

    #[test]
    fn splits_a_stream_into_headers_and_lengths() {
        let s = parse_stream(&stream()).unwrap();
        assert_eq!(s.vps, vec![64, 1, 9, 9]);
        assert_eq!(s.pps, vec![68, 1, 5]);
        assert_eq!(s.data, vec![0, 0, 0, 4, 38, 1, 0xAA, 0xBB]);
    }
    #[test]
    fn a_stream_with_no_picture_is_an_error() {
        assert!(parse_stream(&[0, 0, 1, 32 << 1, 1]).is_err());
    }
    #[test]
    fn profile_comes_out_unescaped() {
        let s = parse_stream(&stream()).unwrap();
        assert_eq!(profile_of(&s.sps).unwrap(), [1, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 0]);
    }
    #[test]
    fn odd_and_tiny_pictures_are_stored_larger() {
        assert_eq!(size_for(101, 50), Size { w: 101, h: 50, coded_w: 102, coded_h: 50 });
        assert_eq!(size_for(3, 3), Size { w: 3, h: 3, coded_w: 16, coded_h: 16 });
        assert_eq!(fit_filter(size_for(100, 50)), "");
        assert!(fit_filter(size_for(101, 50)).contains("right=1:bottom=0"));
    }
    #[test]
    fn items_point_at_their_data() {
        let s = parse_stream(&stream()).unwrap();
        let f = mux(&s, Some(&s), size_for(100, 50), None).unwrap();
        assert_eq!(&f[4..12], b"ftypheic");
        let at = f.windows(4).position(|w| w == b"iloc").unwrap() + 4 + 4 + 4;
        let (off1, len1) = (u32::from_be_bytes(f[at + 6..at + 10].try_into().unwrap()) as usize, u32::from_be_bytes(f[at + 10..at + 14].try_into().unwrap()) as usize);
        assert_eq!(&f[off1..off1 + len1], &s.data[..]);
        let mdat = f.windows(4).rposition(|w| w == b"mdat").unwrap();
        assert_eq!(off1, mdat + 4);
    }
    #[test]
    fn crop_states_the_size_on_screen_from_the_centre() {
        let c = clap(size_for(101, 50));
        let read = |at: usize| i32::from_be_bytes(c[8 + at * 4..12 + at * 4].try_into().unwrap());
        assert_eq!((read(0), read(1), read(2), read(3)), (101, 1, 50, 1));
        assert_eq!((read(4), read(5), read(6), read(7)), (-1, 2, 0, 2));
    }
    #[test]
    fn colour_box_is_the_right_length() {
        let c = colr_nclx(false);
        assert_eq!(c.len(), 8 + 4 + 7);
        assert_eq!(&c[12..19], &[0, 1, 0, 13, 0, 1, 0]);
    }
}
