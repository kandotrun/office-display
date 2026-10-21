use std::collections::HashSet;
pub const MAX_JSON_BYTES: usize = 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Playlist {
    pub photos: Vec<String>,
    pub skipped_videos: usize,
    pub rejected_entries: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Photo,
    Video,
}

pub fn kind(name: &str) -> Option<Kind> {
    if name.is_empty()
        || name.len() > 255
        || name.starts_with('.')
        || name.contains("..")
        || name
            .chars()
            .any(|c| c.is_control() || "/\\%?#:".contains(c))
    {
        return None;
    }
    let (_, ext) = name.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "png" => Some(Kind::Photo),
        "mp4" | "webm" | "mov" | "m4v" | "mkv" | "avi" => Some(Kind::Video),
        _ => None,
    }
}
pub fn parse(bytes: &[u8]) -> Result<Playlist, &'static str> {
    if bytes.len() > MAX_JSON_BYTES {
        return Err("playlist_too_large");
    }
    let names: Vec<String> = serde_json::from_slice(bytes).map_err(|_| "invalid_playlist_json")?;
    if names.len() > MAX_ENTRIES {
        return Err("too_many_entries");
    }
    let mut p = Playlist::default();
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name.clone()) {
            p.rejected_entries += 1;
            continue;
        }
        match kind(&name) {
            Some(Kind::Photo) => p.photos.push(name),
            Some(Kind::Video) => p.skipped_videos += 1,
            None => p.rejected_entries += 1,
        }
    }
    Ok(p)
}
/// Strict basename validation precedes UTF-8 percent escaping. No URL/path input exists.
pub fn photo_path(name: &str) -> Result<String, &'static str> {
    if kind(name) != Some(Kind::Photo) {
        return Err("invalid_photo_name");
    }
    let mut path = String::from("/photos/");
    use std::fmt::Write;
    for b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(b) {
            path.push(*b as char);
        } else {
            let _ = write!(path, "%{b:02X}");
        }
    }
    Ok(path)
}
pub fn batch_indices(length: usize, cursor: usize) -> Vec<usize> {
    if length == 0 {
        return vec![];
    }
    (0..length.min(4)).map(|i| (cursor + i) % length).collect()
}
/// Commit only a validated empty response, or a nonempty response with a usable scene.
pub fn commit_refresh(current: &mut Playlist, candidate: Playlist, loaded: usize) -> bool {
    if !candidate.photos.is_empty() && loaded == 0 {
        return false;
    }
    *current = candidate;
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_paths_queries_traversal_and_unsupported() {
        for bad in [
            "../x.jpg",
            "a/1.jpg",
            "a\\b.png",
            "x.jpg?q=1",
            "a%2F.jpg",
            "http:x.jpg",
            "..jpg",
            "x.gif",
            ".hidden.jpg",
            "x\n.jpg",
        ] {
            assert_eq!(kind(bad), None, "invalid input");
        }
        assert_eq!(kind("普通の写真.JPG"), Some(Kind::Photo));
        assert_eq!(photo_path("a b.jpg").unwrap(), "/photos/a%20b.jpg");
    }
    #[test]
    fn filters_videos_and_deduplicates() {
        let p = parse(br#"["a.jpg","b.MP4","c.mov","../bad.jpg","a.jpg","d.png"]"#).unwrap();
        assert_eq!(p.photos.len(), 2);
        assert_eq!(p.skipped_videos, 2);
        assert_eq!(p.rejected_entries, 2);
        assert!(parse(br#"["a.jpg", 4]"#).is_err());
        assert!(parse(b"{}").is_err());
    }
    #[test]
    fn wraps_without_duplicates_when_small() {
        assert_eq!(batch_indices(5, 4), vec![4, 0, 1, 2]);
        assert_eq!(batch_indices(2, 1), vec![1, 0]);
        assert!(batch_indices(0, 1).is_empty());
    }
    #[test]
    fn failures_retain_but_valid_empty_clears() {
        let mut old = parse(br#"["a.jpg"]"#).unwrap();
        let next = parse(br#"["b.jpg"]"#).unwrap();
        assert!(!commit_refresh(&mut old, next.clone(), 0));
        assert_eq!(old.photos, vec!["a.jpg"]);
        assert!(commit_refresh(&mut old, next, 1));
        assert_eq!(old.photos, vec!["b.jpg"]);
        assert!(commit_refresh(&mut old, parse(b"[]").unwrap(), 0));
        assert!(old.photos.is_empty());
    }
    #[test]
    fn bounds_json_and_entries() {
        assert!(parse(&vec![b' '; MAX_JSON_BYTES + 1]).is_err());
        let too_many = serde_json::to_vec(&vec!["a.jpg"; MAX_ENTRIES + 1]).unwrap();
        assert!(parse(&too_many).is_err());
    }
}
