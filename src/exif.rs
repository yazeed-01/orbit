//! Minimal EXIF reader (JPEG APP1 / raw TIFF): camera, date, exposure, GPS.

#[derive(Default)]
pub struct Exif {
    pub make: Option<String>,
    pub model: Option<String>,
    pub date: Option<String>,
    pub exposure: Option<String>,
    pub fnumber: Option<f64>,
    pub iso: Option<u32>,
    pub focal: Option<f64>,
    pub gps: Option<(f64, f64)>,
}

struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

impl Tiff<'_> {
    fn u16(&self, o: usize) -> Option<u16> {
        let b = self.d.get(o..o + 2)?;
        Some(if self.le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    }
    fn u32(&self, o: usize) -> Option<u32> {
        let b = self.d.get(o..o + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    }
    /// (tag, type, count, offset-of-value-field)
    fn entries(&self, ifd: usize) -> Vec<(u16, u16, u32, usize)> {
        let Some(n) = self.u16(ifd) else { return vec![] };
        (0..n as usize)
            .filter_map(|i| {
                let e = ifd + 2 + i * 12;
                Some((self.u16(e)?, self.u16(e + 2)?, self.u32(e + 4)?, e + 8))
            })
            .collect()
    }
    fn value_at(&self, typ: u16, count: u32, field: usize) -> usize {
        let size = match typ {
            1 | 2 | 7 => 1,
            3 => 2,
            4 | 9 => 4,
            5 | 10 => 8,
            _ => 1,
        } * count as usize;
        if size <= 4 { field } else { self.u32(field).unwrap_or(0) as usize }
    }
    fn ascii(&self, typ: u16, count: u32, field: usize) -> Option<String> {
        let o = self.value_at(typ, count, field);
        let b = self.d.get(o..o + count as usize)?;
        let s = String::from_utf8_lossy(b).trim_matches(char::from(0)).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
    fn rational(&self, o: usize) -> Option<(u32, u32)> {
        Some((self.u32(o)?, self.u32(o + 4)?))
    }
    fn num(&self, typ: u16, count: u32, field: usize) -> Option<f64> {
        let o = self.value_at(typ, count, field);
        match typ {
            3 => self.u16(o).map(f64::from),
            4 => self.u32(o).map(f64::from),
            5 | 10 => self.rational(o).filter(|r| r.1 != 0).map(|r| r.0 as f64 / r.1 as f64),
            _ => None,
        }
    }
    fn dms(&self, typ: u16, count: u32, field: usize) -> Option<f64> {
        let o = self.value_at(typ, count, field);
        let part = |i: usize| self.rational(o + i * 8).filter(|r| r.1 != 0).map(|r| r.0 as f64 / r.1 as f64);
        Some(part(0)? + part(1)? / 60.0 + part(2)? / 3600.0)
    }
}

fn find_tiff(data: &[u8]) -> Option<&[u8]> {
    if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        return Some(data);
    }
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    while i + 4 < data.len() {
        if data[i] != 0xFF {
            return None;
        }
        let marker = data[i + 1];
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if marker == 0xE1 && data.get(i + 4..i + 10) == Some(b"Exif\0\0") {
            return data.get(i + 10..i + 2 + len);
        }
        if marker == 0xDA {
            return None;
        }
        i += 2 + len;
    }
    None
}

pub fn read(data: &[u8]) -> Option<Exif> {
    let d = find_tiff(data)?;
    let t = Tiff { d, le: d.starts_with(b"II") };
    let ifd0 = t.u32(4)? as usize;
    let mut x = Exif::default();
    let (mut exif_ifd, mut gps_ifd) = (None, None);
    for (tag, typ, count, f) in t.entries(ifd0) {
        match tag {
            0x010F => x.make = t.ascii(typ, count, f),
            0x0110 => x.model = t.ascii(typ, count, f),
            0x0132 => x.date = x.date.take().or_else(|| t.ascii(typ, count, f)),
            0x8769 => exif_ifd = t.u32(f).map(|v| v as usize),
            0x8825 => gps_ifd = t.u32(f).map(|v| v as usize),
            _ => {}
        }
    }
    if let Some(ifd) = exif_ifd {
        for (tag, typ, count, f) in t.entries(ifd) {
            match tag {
                0x9003 => x.date = t.ascii(typ, count, f).or(x.date.take()),
                0x829A => {
                    let o = t.value_at(typ, count, f);
                    x.exposure = t.rational(o).filter(|r| r.1 != 0).map(|(n, d)| {
                        let v = n as f64 / d as f64;
                        if v >= 1.0 { format!("{v:.1}s") } else { format!("1/{}s", (1.0 / v).round() as u32) }
                    });
                }
                0x829D => x.fnumber = t.num(typ, count, f),
                0x8827 => x.iso = t.num(typ, count, f).map(|v| v as u32),
                0x920A => x.focal = t.num(typ, count, f),
                _ => {}
            }
        }
    }
    if let Some(ifd) = gps_ifd {
        let (mut lat, mut lon, mut lat_ref, mut lon_ref) = (None, None, 'N', 'E');
        for (tag, typ, count, f) in t.entries(ifd) {
            match tag {
                1 => lat_ref = t.d.get(f).map(|&b| b as char).unwrap_or('N'),
                2 => lat = t.dms(typ, count, f),
                3 => lon_ref = t.d.get(f).map(|&b| b as char).unwrap_or('E'),
                4 => lon = t.dms(typ, count, f),
                _ => {}
            }
        }
        if let (Some(a), Some(b)) = (lat, lon) {
            if a != 0.0 || b != 0.0 {
                x.gps = Some((if lat_ref == 'S' { -a } else { a }, if lon_ref == 'W' { -b } else { b }));
            }
        }
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    /// Little-endian TIFF: IFD0 {Make, Model, GPS ptr}, GPS IFD {N, lat, E, lon}.
    fn sample() -> Vec<u8> {
        let mut d = b"II*\0".to_vec();
        d.extend(8u32.to_le_bytes());
        let entry = |d: &mut Vec<u8>, tag: u16, typ: u16, count: u32, val: u32| {
            d.extend(tag.to_le_bytes());
            d.extend(typ.to_le_bytes());
            d.extend(count.to_le_bytes());
            d.extend(val.to_le_bytes());
        };
        // IFD0 at 8: 3 entries -> 2 + 36 + 4 = 42 bytes, data from 50
        d.extend(3u16.to_le_bytes());
        entry(&mut d, 0x010F, 2, 6, 50); // "Canon\0"
        entry(&mut d, 0x0110, 2, 12, 56); // "Canon EOS R\0"
        entry(&mut d, 0x8825, 4, 1, 68);
        d.extend(0u32.to_le_bytes());
        d.extend(b"Canon\0Canon EOS R\0");
        assert_eq!(d.len(), 68);
        // GPS IFD at 68: 4 entries -> 2 + 48 + 4 = 54 bytes, rationals from 122
        d.extend(4u16.to_le_bytes());
        entry(&mut d, 1, 2, 2, u32::from_le_bytes(*b"N\0\0\0"));
        entry(&mut d, 2, 5, 3, 122);
        entry(&mut d, 3, 2, 2, u32::from_le_bytes(*b"W\0\0\0"));
        entry(&mut d, 4, 5, 3, 146);
        d.extend(0u32.to_le_bytes());
        for (n, den) in [(31, 1), (57, 1), (1800, 100), (35, 1), (54, 1), (360, 10)] {
            d.extend((n as u32).to_le_bytes());
            d.extend((den as u32).to_le_bytes());
        }
        d
    }

    #[test]
    fn parses_camera_and_gps() {
        let x = super::read(&sample()).unwrap();
        assert_eq!(x.make.as_deref(), Some("Canon"));
        assert_eq!(x.model.as_deref(), Some("Canon EOS R"));
        let (lat, lon) = x.gps.unwrap();
        assert!((lat - 31.955).abs() < 1e-3, "{lat}");
        assert!((lon + 35.91).abs() < 1e-3, "{lon}");
    }
}
