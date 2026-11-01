//! The monospaced font families installed, for Settings ▸ Text ▸ Font, and
//! the files of one family's four styles. `font=<family>` in the settings
//! file picks one; unset, the bundled JetBrains Mono.
//!
//! Linux asks fontconfig (`fc-list`), which knows every font already. macOS
//! and Haiku have no fontconfig: their font folders are read, each face's
//! name table parsed (ttf-parser).

use std::process::Command;
use std::sync::Mutex;

/// One face of a family, in a file (and its place in a collection).
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub path: String,
    pub index: u32,
    pub family: String,
    pub bold: bool,
    pub italic: bool,
}

/// What the rasterizer loads for a family: regular, bold, italic and bold
/// italic, each as (file contents, face index); a missing style is regular.
pub type Styles = [(Vec<u8>, u32); 4];

/// The installed monospaced faces, read once.
static INDEX: Mutex<Option<Vec<Face>>> = Mutex::new(None);

fn index() -> Vec<Face> {
    let mut index = crate::Locked::locked(&INDEX);
    index.get_or_insert_with(|| fontconfig().unwrap_or_else(scan)).clone()
}

/// The monospaced families, sorted, each once.
pub fn families() -> Vec<String> {
    let mut families: Vec<String> = index().into_iter().map(|face| face.family).collect();
    families.sort_by_key(|family| family.to_lowercase());
    families.dedup();
    families
}

/// The four styles of `family`, read from disk; None if it isn't installed.
pub fn styles(family: &str) -> Option<Styles> {
    let faces: Vec<Face> = index().into_iter().filter(|face| face.family == family).collect();
    let pick = |bold: bool, italic: bool| {
        faces
            .iter()
            .find(|f| f.bold == bold && f.italic == italic)
            .or_else(|| faces.iter().find(|f| !f.bold && !f.italic))
            .or(faces.first())
    };
    let load = |face: &Face| std::fs::read(&face.path).ok().map(|data| (data, face.index));
    let regular = load(pick(false, false)?)?;
    let style = |bold, italic| pick(bold, italic).and_then(load).unwrap_or_else(|| regular.clone());
    Some([style(false, false), style(true, false), style(false, true), style(true, true)])
}

/// Files the rasterizer reads: TrueType and OpenType, not Type 1.
fn readable(path: &str) -> bool {
    let lower = path.to_lowercase();
    [".ttf", ".otf", ".ttc", ".otc"].iter().any(|ext| lower.ends_with(ext))
}

/// fontconfig's monospaced faces, where it's installed.
fn fontconfig() -> Option<Vec<Face>> {
    let output = Command::new("fc-list")
        .args(["-f", "%{file}|%{index}|%{family[0]}|%{weight}|%{slant}\\n", ":spacing=mono"])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let faces = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('|');
            let (path, index, family, weight, slant) =
                (fields.next()?, fields.next()?, fields.next()?, fields.next()?, fields.next()?);
            readable(path).then(|| Face {
                path: path.to_owned(),
                // A variable font's named instance rides in the high bits.
                index: index.parse::<u32>().unwrap_or(0) & 0xffff,
                family: family.to_owned(),
                // fontconfig's scale: 80 regular, 200 bold; slant 0 upright.
                bold: weight.parse::<u32>().unwrap_or(80) >= 180,
                italic: slant.parse::<u32>().unwrap_or(0) > 0,
            })
        })
        .collect();
    Some(faces)
}

/// The font folders of macOS and Haiku (and Linux without fontconfig).
fn font_dirs() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    [
        "/System/Library/Fonts",
        "/Library/Fonts",
        "~/Library/Fonts",
        "/boot/system/data/fonts",
        "/boot/system/non-packaged/data/fonts",
        "~/config/non-packaged/data/fonts",
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "~/.local/share/fonts",
        "~/.fonts",
    ]
    .iter()
    .map(|dir| dir.replacen('~', &home, 1))
    .collect()
}

/// Every monospaced face in the font folders, by reading each file.
fn scan() -> Vec<Face> {
    let mut faces = Vec::new();
    let mut dirs = font_dirs();
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path.to_string_lossy().into_owned());
            } else if readable(&path.to_string_lossy()) {
                if let Ok(data) = std::fs::read(&path) {
                    faces.extend(faces_in(&path.to_string_lossy(), &data));
                }
            }
        }
    }
    faces
}

/// The monospaced faces in one font file.
fn faces_in(path: &str, data: &[u8]) -> Vec<Face> {
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    (0..count)
        .filter_map(|index| {
            let face = ttf_parser::Face::parse(data, index).ok()?;
            if !face.is_monospaced() {
                return None;
            }
            let name = |id| face.names().into_iter().find(|n| n.name_id == id).and_then(|n| n.to_string());
            let family = name(ttf_parser::name_id::TYPOGRAPHIC_FAMILY).or_else(|| name(ttf_parser::name_id::FAMILY))?;
            Some(Face { path: path.to_owned(), index, family, bold: face.is_bold(), italic: face.is_italic() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_faces_read_as_one_monospaced_family() {
        let faces: Vec<Face> = [
            ("Regular", &include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")[..]),
            ("BoldItalic", &include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf")[..]),
        ]
        .iter()
        .flat_map(|(name, data)| faces_in(name, data))
        .collect();
        assert_eq!(faces.len(), 2);
        assert!(faces.iter().all(|face| face.family == "JetBrains Mono"));
        assert_eq!((faces[0].bold, faces[0].italic), (false, false));
        assert_eq!((faces[1].bold, faces[1].italic), (true, true));
    }

    #[test]
    fn only_truetype_and_opentype_load() {
        assert!(readable("/a/Mono.TTF") && readable("/a/b.ttc") && !readable("/a/Nimbus.t1") && !readable("/a/x.pfb"));
    }
}
