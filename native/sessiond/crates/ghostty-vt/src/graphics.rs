//! Preserve visible cursor-placed PNG previews when exporting the active screen as VT.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader};
use std::{
    io::{Cursor, Write},
    ptr, slice,
    sync::OnceLock,
};

const STORAGE: u64 = 16 << 20;
const MAX_REPLAY: usize = 4 << 20;

unsafe extern "C" fn decode_png(
    _: *mut c_void,
    allocator: *const sys::GhosttyAllocator,
    data: *const u8,
    len: usize,
    out: *mut sys::GhosttySysImage,
) -> bool {
    if data.is_null() || out.is_null() || len > MAX_REPLAY {
        return false;
    }
    // libghostty borrows the encoded bytes for this call and owns the result allocated
    // with its allocator. No Rust allocation crosses the callback boundary.
    let png = unsafe { slice::from_raw_parts(data, len) };
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(STORAGE);
    reader.limits(limits);
    let Ok(decoder) = reader.into_decoder() else {
        return false;
    };
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) * 4 > STORAGE || decoder.total_bytes() > STORAGE {
        return false;
    }
    let Ok(img) = DynamicImage::from_decoder(decoder) else {
        return false;
    };
    let pixels = img.into_rgba8().into_raw();
    unsafe {
        let buffer = sys::ghostty_alloc(allocator, pixels.len());
        if buffer.is_null() {
            return false;
        }
        ptr::copy_nonoverlapping(pixels.as_ptr(), buffer, pixels.len());
        out.write(sys::GhosttySysImage {
            width,
            height,
            data: buffer,
            data_len: pixels.len(),
        });
    }
    true
}

struct Iter(sys::GhosttyKittyGraphicsPlacementIterator);
impl Drop for Iter {
    fn drop(&mut self) {
        unsafe { sys::ghostty_kitty_graphics_placement_iterator_free(self.0) }
    }
}

impl<H: Handler> Terminal<H> {
    pub fn enable_inline_images(&mut self) -> Result<()> {
        static DECODER: OnceLock<Result<()>> = OnceLock::new();
        (*DECODER.get_or_init(|| {
            check(unsafe { sys::ghostty_sys_set(sys::GHOSTTY_SYS_OPT_DECODE_PNG, decode_png as *const c_void) })
        }))?;
        let disabled = &false as *const bool;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE, disabled.cast())?;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM, disabled.cast())?;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE, ptr::null())?;
        self.set_ptr(
            sys::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
            (&STORAGE as *const u64).cast(),
        )
    }

    pub fn visible_image_commands(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let Ok(graphics) = self.get::<sys::GhosttyKittyGraphics>(sys::GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS) else {
            return out;
        };
        let Some(iter) = Iter::new(graphics) else { return out };
        let mut remaining_pixels = STORAGE as usize;
        // Geometry and pixel pointers remain valid while this Terminal is immutably borrowed.
        while unsafe { sys::ghostty_kitty_graphics_placement_next(iter.0) } {
            let Some((id, geom, png)) = self.placement_png(&iter, graphics, &mut remaining_pixels) else {
                continue;
            };
            let encoded = STANDARD.encode(png);
            let chunks = encoded.as_bytes().chunks(4096);
            if out.len() + encoded.len() + chunks.len() * 96 > MAX_REPLAY {
                break;
            }
            let _ = write!(out, "\x1b[{};{}H", geom.viewport_row + 1, geom.viewport_col + 1);
            let count = chunks.len();
            for (index, chunk) in chunks.enumerate() {
                out.extend_from_slice(b"\x1b_G");
                if index == 0 {
                    let _ = write!(
                        out,
                        "a=T,t=d,f=100,c={},r={},i={id},q=2,C=1,",
                        geom.grid_cols, geom.grid_rows
                    );
                }
                let _ = write!(out, "m={};", u8::from(index + 1 < count));
                out.extend_from_slice(chunk);
                out.extend_from_slice(b"\x1b\\");
            }
        }
        out
    }

    fn placement_png(
        &self,
        iter: &Iter,
        graphics: sys::GhosttyKittyGraphics,
        budget: &mut usize,
    ) -> Option<(u32, sys::GhosttyKittyGraphicsPlacementRenderInfo, Vec<u8>)> {
        let mut id = 0u32;
        let mut geom = sys::GhosttyKittyGraphicsPlacementRenderInfo {
            size: size_of::<sys::GhosttyKittyGraphicsPlacementRenderInfo>(),
            ..Default::default()
        };
        let image = unsafe {
            check(sys::ghostty_kitty_graphics_placement_get(
                iter.0,
                sys::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IMAGE_ID,
                (&mut id as *mut u32).cast(),
            ))
            .ok()?;
            let image = sys::ghostty_kitty_graphics_image(graphics, id);
            if image.is_null() {
                return None;
            }
            check(sys::ghostty_kitty_graphics_placement_render_info(
                iter.0, image, self.raw, &mut geom,
            ))
            .ok()?;
            image
        };
        if !geom.viewport_visible || geom.viewport_col < 0 || geom.viewport_row < 0 {
            return None;
        }
        // Codex sends whole RGBA thumbnails. Match libghostty's documented output type
        // for each key; the pixel borrow ends before advancing the placement iterator.
        let mut w = 0u32;
        let mut h = 0u32;
        let mut format: sys::GhosttyKittyImageFormat = 0;
        let mut pixels: *const u8 = ptr::null();
        let mut len = 0usize;
        let keys = [
            sys::GHOSTTY_KITTY_IMAGE_DATA_WIDTH,
            sys::GHOSTTY_KITTY_IMAGE_DATA_HEIGHT,
            sys::GHOSTTY_KITTY_IMAGE_DATA_FORMAT,
            sys::GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR,
            sys::GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN,
        ];
        let mut values = [
            (&mut w as *mut u32).cast(),
            (&mut h as *mut u32).cast(),
            (&mut format as *mut sys::GhosttyKittyImageFormat).cast(),
            (&mut pixels as *mut *const u8).cast(),
            (&mut len as *mut usize).cast(),
        ];
        check(unsafe {
            sys::ghostty_kitty_graphics_image_get_multi(
                image,
                keys.len(),
                keys.as_ptr(),
                values.as_mut_ptr(),
                ptr::null_mut(),
            )
        })
        .ok()?;
        let expected = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
        if format != sys::GHOSTTY_KITTY_IMAGE_FORMAT_RGBA
            || pixels.is_null()
            || len != expected
            || geom.source_width != w
            || geom.source_height != h
        {
            return None;
        }
        *budget = budget.checked_sub(len)?;
        let pixels = unsafe { slice::from_raw_parts(pixels, len) };
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(pixels, w, h, image::ExtendedColorType::Rgba8)
            .ok()?;
        Some((id, geom, png))
    }
}

impl Iter {
    fn new(graphics: sys::GhosttyKittyGraphics) -> Option<Self> {
        let mut handle = ptr::null_mut();
        unsafe {
            check(sys::ghostty_kitty_graphics_placement_iterator_new(
                ptr::null(),
                &mut handle,
            ))
            .ok()?;
            let iter = Iter(handle);
            check(sys::ghostty_kitty_graphics_get(
                graphics,
                sys::GHOSTTY_KITTY_GRAPHICS_DATA_PLACEMENT_ITERATOR,
                (&mut handle as *mut sys::GhosttyKittyGraphicsPlacementIterator).cast(),
            ))
            .ok()?;
            Some(iter)
        }
    }
}
