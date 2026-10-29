//! Built-in viewers: a clicked link opens beside its pane, sliding in from
//! the right. Text files open in an editor, web pages in a browser, images
//! in an image viewer. They are native views (src/macos/shim.m) laid over
//! a pane's place; this is the app's side of one, and what a link opens in.

use std::path::Path;

pub use crate::platform::ViewerKind as Kind;

/// Text files bigger than this open in the system's app.
const MAX_TEXT: u64 = 16 << 20;

pub struct Viewer {
    pub id: u64,
    pub kind: Kind,
    /// The file's path, or the page's address as last navigated.
    pub target: String,
    pub title: String,
    pub dirty: bool,
    /// Command+W with unsaved changes warns once; the next one discards.
    pub close_armed: bool,
}

impl Viewer {
    pub fn new(id: u64, kind: Kind, target: String) -> Viewer {
        let title = title_of(&target);
        Viewer { id, kind, target, title, dirty: false, close_armed: false }
    }

    /// Shows `target` instead, as if just opened (without sliding again).
    pub fn retarget(&mut self, target: String) {
        self.title = title_of(&target);
        self.target = target;
        self.dirty = false;
        self.close_armed = false;
    }
}

/// A file's name, or a page's host, until the page says its title.
fn title_of(target: &str) -> String {
    if let Some(rest) = target.split_once("://").map(|(_, rest)| rest) {
        return rest.split('/').next().unwrap_or(rest).to_owned();
    }
    Path::new(target).file_name().map_or_else(|| target.to_owned(), |name| name.to_string_lossy().into_owned())
}

/// What a clicked link opens in, and the path or address to give it; None
/// for the system's app (folders, binaries, other schemes).
pub fn classify(url: &str) -> Option<(Kind, String)> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some((Kind::Web, url.to_owned()));
    }
    let path = match url.strip_prefix("file://") {
        // file://host/path: the path, from its first slash.
        Some(rest) => crate::session::percent_decode(&rest[rest.find('/')?..]),
        None if url.starts_with('/') => url.to_owned(),
        None => return None,
    };
    let extension = Path::new(&path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    let kind = match extension.as_deref().unwrap_or("") {
        // An RTFD is a folder.
        "rtfd" if Path::new(&path).is_dir() => Kind::Text,
        _ if !Path::new(&path).is_file() => return None,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "tif" | "tiff" | "bmp" | "ico" | "icns" => {
            Kind::Image
        }
        "svg" | "pdf" | "html" | "htm" => Kind::Web,
        "rtf" => Kind::Text,
        _ if is_text(Path::new(&path)) => Kind::Text,
        _ => return None,
    };
    Some((kind, path))
}

/// Whether a file reads as UTF-8 text: small enough, no NULs in its start.
fn is_text(path: &Path) -> bool {
    use std::io::Read;
    if !std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_TEXT) {
        return false;
    }
    let mut head = Vec::with_capacity(8192);
    let Ok(file) = std::fs::File::open(path) else { return false };
    if file.take(8192).read_to_end(&mut head).is_err() {
        return false;
    }
    if head.contains(&0) {
        return false;
    }
    // A character cut off at the end of the sample still counts.
    match std::str::from_utf8(&head) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_open_in_the_right_viewer() {
        let root = env!("CARGO_MANIFEST_DIR");
        assert_eq!(classify("https://example.com/a").map(|(k, _)| k), Some(Kind::Web));
        let cargo = format!("{root}/Cargo.toml");
        assert_eq!(classify(&cargo), Some((Kind::Text, cargo.clone())));
        assert_eq!(classify(&format!("file://localhost{cargo}")), Some((Kind::Text, cargo)));
        let icon = format!("{root}/assets/fonts/JetBrainsMonoNL-Regular.ttf");
        assert_eq!(classify(&icon), None, "binary files go to the system");
        assert_eq!(classify(root), None, "folders go to the system");
        assert_eq!(classify("mailto:someone@example.com"), None);
    }

    #[test]
    fn titles_start_from_the_file_or_host() {
        assert_eq!(title_of("https://example.com/a/b"), "example.com");
        assert_eq!(title_of("/tmp/notes.md"), "notes.md");
    }
}
