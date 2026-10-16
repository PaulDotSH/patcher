//! Branding Windows stubs: the mod's icon and version information, written into the stub's
//! resources before the bundle and patch are appended.

use std::path::Path;

use anyhow::{Context, Result};
use editpe::types::{VersionU16, VersionU32};
use editpe::{Image, ResourceDirectory, VersionStringTable};
use image::{DynamicImage, GenericImageView, RgbaImage};
use patcher_core::format::Meta;

/// `1.2.3-beta` -> (1, 2, 3, 0): leading numbers of up to four dot-separated parts.
fn version_numbers(v: &str) -> [u16; 4] {
    let mut out = [0u16; 4];
    for (i, part) in v.split('.').take(4).enumerate() {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        out[i] = digits.parse().unwrap_or(0);
    }
    out
}

/// Pads a non-square image to a centred square, so Windows does not stretch it.
fn square(img: DynamicImage) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w == h {
        return img;
    }
    let side = w.max(h);
    let mut canvas = RgbaImage::new(side, side);
    image::imageops::overlay(&mut canvas, &img.to_rgba8(), i64::from((side - w) / 2), i64::from((side - h) / 2));
    DynamicImage::ImageRgba8(canvas)
}

/// Returns `stub` with `icon` (PNG or JPEG) as its main icon and `meta` as its version
/// information.
pub fn brand_windows_stub(stub: &[u8], icon: Option<&Path>, meta: &Meta) -> Result<Vec<u8>> {
    let mut img = Image::parse(stub).context("reading the Windows stub")?;
    let mut res: ResourceDirectory = img.resource_directory().cloned().unwrap_or_default();
    if let Some(path) = icon {
        let picture = image::open(path).with_context(|| format!("reading icon {}", path.display()))?;
        res.set_main_icon(square(picture)).context("setting the icon")?;
    }
    let mut info = res.get_version_info().ok().flatten().unwrap_or_default();
    let [a, b, c, d] = version_numbers(&meta.version);
    let v = VersionU32 { major: u32::from(a) << 16 | u32::from(b), minor: u32::from(c) << 16 | u32::from(d) };
    info.info.file_version = v;
    info.info.product_version = v;
    let mut strings = indexmap::IndexMap::with_hasher(foldhash::fast::RandomState::default());
    let name = if meta.name.is_empty() { "Mod" } else { meta.name.as_str() };
    for (k, val) in [
        ("CompanyName", meta.author.as_str()),
        ("FileDescription", &format!("{name} patcher")),
        ("FileVersion", meta.version.as_str()),
        ("ProductName", name),
        ("ProductVersion", meta.version.as_str()),
    ] {
        if !val.is_empty() {
            strings.insert(k.to_string(), val.to_string());
        }
    }
    // US English, Unicode.
    info.strings = vec![VersionStringTable { key: "040904B0".into(), strings }];
    info.vars = vec![VersionU16 { major: 0x0409, minor: 0x04B0 }];
    res.set_version_info(&info).context("setting version information")?;
    img.set_resource_directory(res).context("writing resources")?;
    Ok(img.data().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(version_numbers("1.2.3"), [1, 2, 3, 0]);
        assert_eq!(version_numbers("2.0-beta"), [2, 0, 0, 0]);
        assert_eq!(version_numbers("v3"), [0, 0, 0, 0]);
        assert_eq!(version_numbers("1.2.3.4.5"), [1, 2, 3, 4]);
    }

    #[test]
    fn squares_wide_images() {
        let wide = DynamicImage::new_rgba8(40, 10);
        assert_eq!(square(wide).dimensions(), (40, 40));
    }
}
