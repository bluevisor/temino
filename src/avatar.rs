//! The photos of the people controlling this machine from a phone, shown
//! on the pane each is in. Each is fetched once (with curl, as updates
//! are), kept in the settings directory, and cut to a circle.

use crate::Locked;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::picture::Picture;

/// Pixels a side: the pill draws it at 20 points, so this is sharp at 4x.
const SIZE: u32 = 96;
/// Bigger downloads aren't a profile photo.
const MOST: u64 = 2 * 1024 * 1024;

/// By address: the photo, or None while it comes (or if it couldn't).
static PHOTOS: Mutex<Option<HashMap<String, Option<Arc<Picture>>>>> = Mutex::new(None);

/// The photo at `url`, once it's here.
pub fn get(url: &str) -> Option<Arc<Picture>> {
    PHOTOS.locked().as_ref()?.get(url).cloned().flatten()
}

/// Starts fetching the photo at `url` (an https address), once.
pub fn fetch(url: &str) {
    {
        let mut photos = PHOTOS.locked();
        let photos = photos.get_or_insert_with(HashMap::new);
        if photos.contains_key(url) {
            return;
        }
        photos.insert(url.to_owned(), None);
    }
    let url = url.to_owned();
    let _ = std::thread::Builder::new().name("avatar".into()).spawn(move || {
        let photo = load(&url);
        if let Some(photos) = PHOTOS.locked().as_mut() {
            photos.insert(url, photo);
        }
        crate::remote::refresh();
    });
}

fn load(url: &str) -> Option<Arc<Picture>> {
    let dir = format!("{}/avatars", crate::settings_dir());
    let path = format!("{dir}/{:016x}", hash(url));
    if !std::path::Path::new(&path).exists() {
        crate::create_private_dir(&dir);
        let partial = format!("{path}.part");
        let fetched = Command::new("curl")
            .args(["-fsSL", "--proto", "=https", "--max-time", "20", "--max-filesize", &MOST.to_string(), "-o"])
            .arg(&partial)
            .arg(url)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !fetched || std::fs::rename(&partial, &path).is_err() {
            let _ = std::fs::remove_file(&partial);
            return None;
        }
    }
    circle(&std::fs::read(&path).ok()?)
}

/// The middle square of the image, SIZE a side, outside the circle clear.
fn circle(bytes: &[u8]) -> Option<Arc<Picture>> {
    let image = image::load_from_memory(bytes).ok()?;
    let side = image.width().min(image.height());
    let square = image.crop_imm((image.width() - side) / 2, (image.height() - side) / 2, side, side);
    let mut rgba = square.resize_exact(SIZE, SIZE, image::imageops::FilterType::Triangle).into_rgba8().into_raw();
    let r = SIZE as f32 / 2.0;
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i as u32 % SIZE) as f32 + 0.5 - r, (i as u32 / SIZE) as f32 + 0.5 - r);
        // A pixel's worth of soft edge.
        let inside = (r - (x * x + y * y).sqrt()).clamp(0.0, 1.0);
        px[3] = (px[3] as f32 * inside) as u8;
    }
    Picture::new(SIZE as usize, SIZE as usize, rgba)
}

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_photo_is_cut_to_a_circle() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(200, 120, image::Rgba([200, 100, 50, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let photo = circle(&png).unwrap();
        assert_eq!((photo.width, photo.height), (SIZE as usize, SIZE as usize));
        let alpha = |x: usize, y: usize| photo.rgba[(y * SIZE as usize + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "a corner is outside the circle");
        assert_eq!(alpha(48, 48), 255, "the middle is inside");
    }
}
