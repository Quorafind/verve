//! The family names a font file gives itself, read from its `name` table.
//!
//! GPUI registers a face under the family it names inside, not under the CSS alias a page gives
//! it, so the measurer needs that name. Reading it is a few bytes of an OpenType table, which
//! costs less than a font parser.

/// `name` table ids: the typographic family, and the family of the four-style group.
const TYPOGRAPHIC_FAMILY: u16 = 16;
const FAMILY: u16 = 1;
/// The Windows platform, whose strings are UTF-16BE, and US English among its languages.
const WINDOWS: u16 = 3;
const ENGLISH_US: u16 = 0x409;

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn be32(bytes: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// The offset of the table `tag` in the (first) font of `bytes`.
fn table(bytes: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    let start = if bytes.get(..4)? == b"ttcf" {
        be32(bytes, 12)?
    } else {
        0
    };
    let count = usize::from(be16(bytes, start + 4)?);
    (0..count).find_map(|index| {
        let record = start + 12 + index * 16;
        (bytes.get(record..record + 4)? == tag)
            .then(|| Some((be32(bytes, record + 8)?, be32(bytes, record + 12)?)))?
    })
}

/// The family names of the font in `bytes`, the ones DirectWrite may know it by, most likely
/// first: the typographic family, then the style group's, each in US English before any other
/// language. Empty when the bytes are not an OpenType font with a name table.
pub fn family_names(bytes: &[u8]) -> Vec<String> {
    let Some((offset, length)) = table(bytes, b"name") else {
        return Vec::new();
    };
    let Some(name) = bytes.get(offset..offset.saturating_add(length).min(bytes.len())) else {
        return Vec::new();
    };
    let (Some(count), Some(strings)) = (be16(name, 2), be16(name, 4)) else {
        return Vec::new();
    };
    let mut found: Vec<(u16, bool, String)> = Vec::new();
    for index in 0..usize::from(count) {
        let record = 6 + index * 12;
        let field = |at: usize| be16(name, record + at);
        let (Some(platform), Some(language), Some(id), Some(size), Some(start)) =
            (field(0), field(4), field(6), field(8), field(10))
        else {
            break;
        };
        if platform != WINDOWS || (id != FAMILY && id != TYPOGRAPHIC_FAMILY) {
            continue;
        }
        let from = usize::from(strings) + usize::from(start);
        let Some(raw) = name.get(from..from + usize::from(size)) else {
            continue;
        };
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        found.push((id, language != ENGLISH_US, String::from_utf16_lossy(&units)));
    }
    // Typographic before group (16 > 1, so reversed), English before the rest.
    found.sort_by_key(|(id, other, _)| (std::cmp::Reverse(*id), *other));
    let mut names: Vec<String> = Vec::new();
    for (_, _, family) in found {
        if !family.is_empty() && !names.contains(&family) {
            names.push(family);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font of one table: `name`, with the records given as (platform, language, id, text).
    fn font_with(records: &[(u16, u16, u16, &str)]) -> Vec<u8> {
        let mut strings: Vec<u8> = Vec::new();
        let mut entries: Vec<u8> = Vec::new();
        for (platform, language, id, text) in records {
            let encoded: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
            for value in [
                *platform,
                1,
                *language,
                *id,
                encoded.len() as u16,
                strings.len() as u16,
            ] {
                entries.extend(value.to_be_bytes());
            }
            strings.extend(encoded);
        }
        let mut name: Vec<u8> = Vec::new();
        for value in [0u16, records.len() as u16, 6 + entries.len() as u16] {
            name.extend(value.to_be_bytes());
        }
        name.extend(entries);
        name.extend(strings);
        let mut font: Vec<u8> = vec![0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        font.extend(b"name");
        font.extend([0u8; 4]);
        font.extend(28u32.to_be_bytes());
        font.extend((name.len() as u32).to_be_bytes());
        font.extend(name);
        font
    }

    #[test]
    fn the_typographic_family_comes_before_the_groups_and_english_before_the_rest() {
        let font = font_with(&[
            (3, 0x804, 1, "微软雅黑"),
            (3, ENGLISH_US, 1, "Brand Light"),
            (1, 0, 1, "Mac Name"),
            (3, ENGLISH_US, 16, "Brand"),
        ]);
        assert_eq!(family_names(&font), ["Brand", "Brand Light", "微软雅黑"]);
    }

    #[test]
    fn bytes_that_are_no_font_have_no_names() {
        assert!(family_names(&[0; 16]).is_empty());
        assert!(family_names(b"").is_empty());
    }
}
