//! Direct PNG transfers and cursor placements used by Codex. Placement anchors live in the
//! grid so they follow scrolling, alternate screens, and erasure with their cells.
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Weak};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};

use super::cell::Cell;
use crate::grid::{Dimensions, Grid};
use crate::index::{Line, Point};

const MAX_TRANSFER: usize = 4 * 1024 * 1024;
const MAX_PIXELS_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMAGES: usize = 64;

struct Transfer {
    id: u32,
    cols: u16,
    rows: u16,
    bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct Placement {
    pub pixels: Arc<RgbaImage>,
    pub cols: u16,
    pub rows: u16,
}

// The grid only holds weak references. Deleting or moving a placement invalidates its
// old anchors, including those in scrollback, without scanning the whole terminal.
#[derive(Clone, Debug)]
pub(super) struct Anchor(pub Weak<Placement>);

impl PartialEq for Anchor {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}
impl Eq for Anchor {}

struct StoredImage {
    pixels: Arc<RgbaImage>,
    placement: Option<Arc<Placement>>,
}

#[derive(Default)]
pub(super) struct Graphics {
    pending: Option<Transfer>,
    images: HashMap<u32, StoredImage>,
}

impl Graphics {
    fn place(&mut self, id: u32, cols: u16, rows: u16, grid: &mut Grid<Cell>) {
        let Some(image) = self.images.get_mut(&id) else {
            return;
        };
        let placement = Arc::new(Placement {
            pixels: image.pixels.clone(),
            cols,
            rows,
        });
        let start = grid.cursor.point;
        let visible_rows = (grid.screen_lines() - start.line.0 as usize).min(usize::from(rows));
        for row in 0..visible_rows {
            let point = Point::new(Line(start.line.0 + row as i32), start.column);
            grid[point].set_image_anchor(Anchor(Arc::downgrade(&placement)), row as u16);
        }
        image.placement = Some(placement);
    }

    pub(super) fn command(&mut self, data: &[u8], grid: &mut Grid<Cell>) {
        let Some(data) = data.strip_prefix(b"G") else {
            return;
        };
        let Some(separator) = data.iter().position(|b| *b == b';') else {
            return;
        };
        let Ok(header) = std::str::from_utf8(&data[..separator]) else {
            return;
        };
        let payload = &data[separator + 1..];
        let field = |key| {
            header
                .split(',')
                .filter_map(|pair| pair.split_once('='))
                .find_map(|(k, v)| (k == key).then_some(v))
        };
        let more = field("m") == Some("1");
        if field("a").is_some() {
            self.pending = None;
            let Some(id) = field("i").and_then(|s| s.parse::<u32>().ok()) else {
                return;
            };
            let extent = |key| field(key)?.parse::<u16>().ok().filter(|n| *n > 0);
            match field("a") {
                Some("d") if matches!(field("d"), Some("i" | "I")) => {
                    if field("d") == Some("I") {
                        self.images.remove(&id);
                    } else if let Some(image) = self.images.get_mut(&id) {
                        image.placement = None;
                    }
                    return;
                }
                Some("T" | "p") if field("U").is_none() => {
                    let (Some(cols), Some(rows)) = (extent("c"), extent("r")) else {
                        return;
                    };
                    if field("a") == Some("p") {
                        self.place(id, cols, rows, grid);
                        return;
                    }
                    if field("f") != Some("100") || !matches!(field("t"), None | Some("d")) {
                        return;
                    }
                    self.pending = Some(Transfer {
                        id,
                        cols,
                        rows,
                        bytes: Vec::new(),
                    });
                }
                _ => return,
            }
        } else if field("m").is_none() {
            return;
        }
        let Some(mut transfer) = self.pending.take() else {
            return;
        };
        if transfer.bytes.len() + payload.len() > MAX_TRANSFER {
            return;
        }
        transfer.bytes.extend_from_slice(payload);
        if more {
            self.pending = Some(transfer);
            return;
        }

        let Ok(png) = STANDARD.decode(transfer.bytes) else {
            return;
        };
        let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(MAX_PIXELS_BYTES as u64);
        reader.limits(limits);
        let Ok(decoder) = reader.into_decoder() else {
            return;
        };
        let (width, height) = decoder.dimensions();
        let bytes = u64::from(width) * u64::from(height);
        let used: usize = self
            .images
            .iter()
            .filter(|(id, _)| **id != transfer.id)
            .map(|(_, img)| img.pixels.as_raw().len())
            .sum();
        if decoder.total_bytes() > MAX_PIXELS_BYTES as u64
            || bytes > ((MAX_PIXELS_BYTES - used) / 4) as u64
            || (!self.images.contains_key(&transfer.id) && self.images.len() >= MAX_IMAGES)
        {
            return;
        }
        let Ok(img) = DynamicImage::from_decoder(decoder) else {
            return;
        };
        self.images.insert(
            transfer.id,
            StoredImage {
                pixels: Arc::new(img.into_rgba8()),
                placement: None,
            },
        );
        self.place(transfer.id, transfer.cols, transfer.rows, grid);
    }
}
