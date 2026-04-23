//! Rasterise the tray SVGs at startup so `ksni::Tray::icon_pixmap`
//! can serve theme-independent ARGB32 bytes straight to the
//! StatusNotifier host. Bypasses KDE's symbolic-icon recolour
//! pipeline, which otherwise collapses the Inkscape-era masked SVGs
//! into a solid black square.
//!
//! Rendering happens exactly once, via `resvg` in pure-Rust mode (no
//! fontdb / text support — these icons are shape-only). Output
//! sizes match the typical StatusNotifier consumption range; the host
//! picks the closest.

use ksni::Icon;
use resvg::{tiny_skia, usvg};

/// One pre-rasterised set per tray state. Cloning a `Vec<Icon>`
/// requires cloning the pixel buffers, so we keep the set alive for
/// the lifetime of the tray service and hand out `.clone()`s on
/// demand.
pub(super) struct IconSet {
    pub loading: Vec<Icon>,
    pub disconnected: Vec<Icon>,
    pub paused: Vec<Icon>,
    pub syncing: Vec<Icon>,
    pub warning: Vec<Icon>,
    pub done: Vec<Icon>,
}

const SIZES: &[u32] = &[16, 22, 24, 32, 48, 64];

macro_rules! embed {
    ($name:literal) => {
        include_bytes!(concat!(
            "../../../assets/context/com.hunterwittenborn.Celeste.CelesteTray",
            $name,
            "-symbolic.svg"
        ))
    };
}

impl IconSet {
    pub fn load() -> Self {
        Self {
            loading: rasterise(embed!("Loading")),
            disconnected: rasterise(embed!("Disconnected")),
            paused: rasterise(embed!("Paused")),
            syncing: rasterise(embed!("Syncing")),
            warning: rasterise(embed!("Warning")),
            done: rasterise(embed!("Done")),
        }
    }
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
    /// advertised size, with at least one non-transparent pixel. Guards
    /// against a resvg / usvg upgrade silently turning the Inkscape-era
    /// masked SVGs into blank canvases.
    #[test]
    fn every_state_rasterises_to_visible_pixels() {
        let set = IconSet::load();
        let buckets: [(&str, &[Icon]); 6] = [
            ("loading", &set.loading),
            ("disconnected", &set.disconnected),
            ("paused", &set.paused),
            ("syncing", &set.syncing),
            ("warning", &set.warning),
            ("done", &set.done),
        ];
        for (name, icons) in buckets {
            assert_eq!(
                icons.len(),
                SIZES.len(),
                "{name} rasterised fewer sizes than expected"
            );
            for icon in icons {
                assert_eq!(
                    icon.data.len(),
                    (icon.width as usize) * (icon.height as usize) * 4,
                    "{name} @ {}x{} has unexpected buffer length",
                    icon.width,
                    icon.height,
                );
                // At least one pixel must have non-zero alpha —
                // otherwise the tray host will render nothing.
                let any_opaque = icon
                    .data
                    .chunks_exact(4)
                    .any(|px| px[0] != 0);
                assert!(
                    any_opaque,
                    "{name} @ {}x{} rasterised to a fully-transparent pixmap",
                    icon.width,
                    icon.height,
                );
            }
        }
    }
}
