//! The monospaced font families installed, for Settings ▸ Text ▸ Font, and
//! the files of one family's four styles. `font=<family>` in the settings
//! file picks one; unset, the bundled JetBrains Mono.
//!
//! Linux asks fontconfig (`fc-list`), which knows every font already. macOS
//! asks CoreText for the fixed-pitch faces' files, which also finds fonts
//! downloaded on demand and fonts apps activate (Adobe Fonts), and adds SF
//! Mono from Terminal.app. Haiku's font folders are read. Either way each
//! file's name table is parsed (ttf-parser).

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
    /// On the CSS scale: 400 regular, 700 bold.
    pub weight: u16,
}

/// What the rasterizer loads for a family: regular, bold, italic and bold
/// italic, each as (file contents, face index); a missing style is regular.
pub type Styles = [(Vec<u8>, u32); 4];

/// The installed monospaced faces, read once.
static INDEX: Mutex<Option<Vec<Face>>> = Mutex::new(None);

fn index() -> Vec<Face> {
    let mut index = crate::Locked::locked(&INDEX);
    // A Homebrew fontconfig on macOS knows only the font folders, not the
    // fonts CoreText downloads or apps activate: CoreText goes first there.
    let read = if cfg!(target_os = "macos") { scan } else { || fontconfig().unwrap_or_else(scan) };
    index.get_or_insert_with(read).clone()
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
    let load = |face: &Face| std::fs::read(&face.path).ok().map(|data| (data, face.index));
    let regular = load(pick(&faces, false, false)?)?;
    let style = |bold, italic| pick(&faces, bold, italic).and_then(load).unwrap_or_else(|| regular.clone());
    Some([style(false, false), style(true, false), style(false, true), style(true, true)])
}

/// The face of one family drawn for a style: the one nearest its weight,
/// so a family with Light, Medium and Heavy faces too still draws in its
/// Regular and Bold; a style it lacks falls back to regular.
fn pick(faces: &[Face], bold: bool, italic: bool) -> Option<&Face> {
    let nearest = |weight: u16, italic: bool| {
        faces.iter().filter(|f| f.italic == italic).min_by_key(|f| (f.weight as i32 - weight as i32).abs())
    };
    nearest(if bold { 700 } else { 400 }, italic)
        .filter(|f| f.bold == bold)
        .or_else(|| nearest(400, false))
        .or(faces.first())
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
                weight: css_weight(weight.parse::<u32>().unwrap_or(80)),
            })
        })
        .collect();
    Some(faces)
}

/// fontconfig's weight (80 regular, 200 bold) on the CSS scale.
fn css_weight(fontconfig: u32) -> u16 {
    const SCALE: [(u32, u16); 9] =
        [(0, 100), (40, 200), (50, 300), (80, 400), (100, 500), (180, 600), (200, 700), (205, 800), (210, 900)];
    SCALE.iter().rev().find(|&&(at, _)| fontconfig >= at).map_or(400, |&(_, css)| css)
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

/// SF Mono ships inside Terminal.app, not in a font folder.
#[cfg(target_os = "macos")]
const SF_MONO_DIRS: [&str; 2] = [
    "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts",
    "/Applications/Utilities/Terminal.app/Contents/Resources/Fonts",
];

/// Every monospaced face macOS knows of: CoreText's list, and SF Mono.
#[cfg(target_os = "macos")]
fn scan() -> Vec<Face> {
    let mut files = crate::platform::mono_font_files();
    for dir in SF_MONO_DIRS {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        files.extend(entries.flatten().map(|entry| entry.path().to_string_lossy().into_owned()));
    }
    if files.is_empty() {
        return scan_dirs(font_dirs());
    }
    files.sort();
    files.dedup();
    files
        .iter()
        .filter(|path| readable(path))
        .flat_map(|path| std::fs::read(path).map(|data| faces_in(path, &data, true)))
        .flatten()
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn scan() -> Vec<Face> {
    scan_dirs(font_dirs())
}

/// Every monospaced face in `dirs` and the folders in them, by reading
/// each file.
fn scan_dirs(mut dirs: Vec<String>) -> Vec<Face> {
    let mut faces = Vec::new();
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path.to_string_lossy().into_owned());
            } else if readable(&path.to_string_lossy()) {
                if let Ok(data) = std::fs::read(&path) {
                    faces.extend(faces_in(&path.to_string_lossy(), &data, false));
                }
            }
        }
    }
    faces
}

/// A name record's text. Apple's own fonts (Menlo, Monaco, SF Mono) name
/// themselves only in Mac Roman, which ttf-parser leaves undecoded; their
/// names are ASCII, so that's read byte for byte.
fn name_text(name: ttf_parser::name::Name) -> Option<String> {
    let mac_roman = name.platform_id == ttf_parser::PlatformId::Macintosh && name.encoding_id == 0;
    name.to_string()
        .or_else(|| (mac_roman && name.name.is_ascii()).then(|| String::from_utf8_lossy(name.name).into_owned()))
}

/// Its letters all take one width: for faces that are monospaced but don't
/// say so in their `post` table, as Monaco doesn't.
fn even_widths(face: &ttf_parser::Face) -> bool {
    let advance = |c| face.glyph_index(c).and_then(|glyph| face.glyph_hor_advance(glyph));
    let widths: Vec<Option<u16>> = ['i', 'l', 'M', 'W', '0'].into_iter().map(advance).collect();
    widths[0].is_some() && widths.iter().all(|w| *w == widths[0])
}

/// The monospaced faces in one font file. `listed`: the system already
/// called its file fixed-pitch (CoreText), so even letter widths suffice.
fn faces_in(path: &str, data: &[u8], listed: bool) -> Vec<Face> {
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    (0..count)
        .filter_map(|index| {
            let face = ttf_parser::Face::parse(data, index).ok()?;
            if !face.is_monospaced() && !(listed && even_widths(&face)) {
                return None;
            }
            let name = |id| face.names().into_iter().filter(|n| n.name_id == id).find_map(name_text);
            let family = name(ttf_parser::name_id::TYPOGRAPHIC_FAMILY).or_else(|| name(ttf_parser::name_id::FAMILY))?;
            // macOS's own UI faces (".SF NS Mono") are hidden from pickers.
            if family.starts_with('.') {
                return None;
            }
            let weight = face.weight().to_number();
            Some(Face {
                path: path.to_owned(),
                index,
                family,
                bold: face.is_bold() || weight >= 600,
                italic: face.is_italic(),
                weight,
            })
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
        .flat_map(|(name, data)| faces_in(name, data, false))
        .collect();
        assert_eq!(faces.len(), 2);
        assert!(faces.iter().all(|face| face.family == "JetBrains Mono"));
        assert_eq!((faces[0].bold, faces[0].italic), (false, false));
        assert_eq!((faces[1].bold, faces[1].italic), (true, true));
    }

    #[test]
    fn regular_and_bold_are_picked_over_the_other_weights() {
        let face = |weight: u16, italic| Face {
            path: format!("{weight}{italic}"),
            index: 0,
            family: "SF Mono".to_owned(),
            bold: weight >= 600,
            italic,
            weight,
        };
        let faces = [
            face(300, false),
            face(900, false),
            face(500, false),
            face(400, false),
            face(700, false),
            face(600, false),
        ];
        assert_eq!(pick(&faces, false, false).unwrap().weight, 400);
        assert_eq!(pick(&faces, true, false).unwrap().weight, 700);
        // No italics: italic draws upright regular, not some other weight.
        assert_eq!(pick(&faces, false, true).unwrap().weight, 400);
        assert_eq!(pick(&faces, true, true).unwrap().weight, 400);
    }

    #[test]
    fn only_truetype_and_opentype_load() {
        assert!(readable("/a/Mono.TTF") && readable("/a/b.ttc") && !readable("/a/Nimbus.t1") && !readable("/a/x.pfb"));
    }
}
