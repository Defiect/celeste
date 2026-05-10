//! Rasterise the tray status icons at startup so `ksni::Tray::icon_pixmap`
//! can serve theme-independent ARGB32 bytes straight to the
//! StatusNotifier host. Bypasses KDE's symbolic-icon recolour
//! pipeline, which otherwise collapses our coloured glyphs into a
//! solid black square.
//!
//! Icons come from `icondata` (raw inner SVG path data plus a viewBox)
//! and are wrapped in a real `<svg>` document with a flat `fill` so
//! `resvg` produces a single-tone glyph. We pre-rasterise both a
//! light- and a dark-tinted variant per state and pick the one that
//! contrasts with the panel based on the detected system colour
//! scheme — KDE/GNOME tray hosts do not pass theme info through for
//! `icon_pixmap`, so detection is on us.

use ksni::Icon;
use resvg::{tiny_skia, usvg};

/// Tone used for icons placed against dark panels (the icon itself
/// is light).
const LIGHT_TONE: &str = "#e6e6e6";
/// Tone used for icons placed against light panels (the icon itself
/// is dark).
const DARK_TONE: &str = "#2c2c2c";

const SIZES: &[u32] = &[16, 22, 24, 32, 48, 64];

/// Whether the surrounding panel is dark (so we want a light icon)
/// or light (so we want a dark icon).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ColorScheme {
    Dark,
    Light,
}

impl ColorScheme {
    /// Best-effort one-shot detection. Falls back to `Dark` because
    /// the major Linux desktops default to dark panels and a light
    /// icon on a dark panel is the safer mis-render.
    pub fn detect() -> Self {
        match dark_light::detect() {
            dark_light::Mode::Light => Self::Light,
            dark_light::Mode::Dark | dark_light::Mode::Default => Self::Dark,
        }
    }
}

/// Two-tone rasterisation for one icon: a light version (for dark
/// panels) and a dark version (for light panels), each pre-sized to
/// every advertised pixmap dimension.
pub(super) struct ThemedIcon {
    light: Vec<Icon>,
    dark: Vec<Icon>,
}

impl ThemedIcon {
    fn from(icon: icondata::Icon) -> Self {
        Self {
            light: rasterise_icon(icon, LIGHT_TONE),
            dark: rasterise_icon(icon, DARK_TONE),
        }
    }

    /// Hand back the colour variant that contrasts with `scheme`.
    pub fn pick(&self, scheme: ColorScheme) -> Vec<Icon> {
        match scheme {
            ColorScheme::Dark => self.light.clone(),
            ColorScheme::Light => self.dark.clone(),
        }
    }
}

/// One pre-rasterised icon per tray state. Cloning a `Vec<Icon>`
/// requires cloning the pixel buffers, so we keep the set alive for
/// the lifetime of the tray service and hand out `.clone()`s on
/// demand.
pub(super) struct IconSet {
    pub synced: ThemedIcon,
    pub auth_needed: ThemedIcon,
    pub syncing: ThemedIcon,
    pub warning: ThemedIcon,
    pub paused: ThemedIcon,
}

impl IconSet {
    pub fn load() -> Self {
        Self {
            synced: ThemedIcon::from(icondata::TbCloudCheckOutline),
            auth_needed: ThemedIcon::from(icondata::TbCloudLockOutline),
            syncing: ThemedIcon::from(icondata::AiSyncOutlined),
            warning: ThemedIcon::from(icondata::TbCloudExclamationOutline),
            paused: ThemedIcon::from(icondata::TbCloudPauseOutline),
        }
    }
}

fn rasterise_icon(icon: icondata::Icon, color: &str) -> Vec<Icon> {
    let view_box = icon.view_box.unwrap_or("0 0 24 24");
    let data = icon.data;
    let svg_doc = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view_box}" fill="{color}" stroke="{color}">{data}</svg>"##,
    );
    rasterise(svg_doc.as_bytes())
}

fn rasterise(svg_bytes: &[u8]) -> Vec<Icon> {
    let opts = usvg::Options::default();
    let tree = match usvg::Tree::from_data(svg_bytes, &opts) {
        Ok(tree) => tree,
        Err(err) => {
            eprintln!("celeste: tray SVG parse failed ({err}); icon will be blank.");
            return Vec::new();
        }
    };
    let svg_size = tree.size();
    SIZES
        .iter()
        .filter_map(|&size| {
            let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
            let scale_x = size as f32 / svg_size.width();
            let scale_y = size as f32 / svg_size.height();
            let transform = tiny_skia::Transform::from_scale(scale_x, scale_y);
            resvg::render(&tree, transform, &mut pixmap.as_mut());
            Some(Icon {
                width: size as i32,
                height: size as i32,
                data: rgba_premul_to_argb_nonpremul(pixmap.data()),
            })
        })
        .collect()
}

/// tiny-skia emits premultiplied RGBA. The StatusNotifier spec asks
/// for non-premultiplied ARGB32 in network byte order — hence the
/// channel swap plus the division.
fn rgba_premul_to_argb_nonpremul(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    for px in src.chunks_exact(4) {
        let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
        let (r, g, b) = if a == 0 {
            (0, 0, 0)
        } else {
            let a_u = a as u32;
            (
                ((r as u32 * 255 + a_u / 2) / a_u).min(255) as u8,
                ((g as u32 * 255 + a_u / 2) / a_u).min(255) as u8,
                ((b as u32 * 255 + a_u / 2) / a_u).min(255) as u8,
            )
        };
        out.extend_from_slice(&[a, r, g, b]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tray state must produce non-empty pixmap data at every
    /// advertised size, with at least one non-transparent pixel, in
    /// both colour variants. Guards against an icondata or resvg/usvg
    /// upgrade silently turning these glyphs into blank canvases.
    #[test]
    fn every_state_rasterises_to_visible_pixels() {
        let set = IconSet::load();
        let buckets: [(&str, &ThemedIcon); 5] = [
            ("synced", &set.synced),
            ("auth_needed", &set.auth_needed),
            ("syncing", &set.syncing),
            ("warning", &set.warning),
            ("paused", &set.paused),
        ];
        for (name, themed) in buckets {
            for (variant, icons) in [("light", &themed.light), ("dark", &themed.dark)] {
                assert_eq!(
                    icons.len(),
                    SIZES.len(),
                    "{name}/{variant} rasterised fewer sizes than expected"
                );
                for icon in icons {
                    assert_eq!(
                        icon.data.len(),
                        (icon.width as usize) * (icon.height as usize) * 4,
                        "{name}/{variant} @ {}x{} has unexpected buffer length",
                        icon.width,
                        icon.height,
                    );
                    let any_opaque = icon.data.chunks_exact(4).any(|px| px[0] != 0);
                    assert!(
                        any_opaque,
                        "{name}/{variant} @ {}x{} rasterised to a fully-transparent pixmap",
                        icon.width,
                        icon.height,
                    );
                }
            }
        }
    }
}
