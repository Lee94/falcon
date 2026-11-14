//! 高吞吐终端网格绘制器：`falcon_term::Snapshot` → NSH `Painter`。
//!
//! 关键设计与性能优化（对齐 ADR 0015 / ADR 0020 / ADR 0022）：
//! 1. **O(1) 调色板查表**：每帧一次性把 `ResolvedTheme::terminal` 与 `Snapshot::colors`（OSC 4/10/11
//!    运行时改色）压成栈上 256 色 `Hsla` 数组，格内前背景解析零堆分配；
//! 2. **同行背景矩形合并**：相邻同色背景格合并成单条 `fill_rect`，与默认终端底色相同的格不发指令；
//! 3. **盒线与块元素矢量直绘（`U+2500..=U+259F`）**：`─ │ ┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼ ═ ║ ▀ ▄ █ ▌ ▐ ░ ▒ ▓`
//!    直接按格几何画像素对齐的矢量矩形，避免不同行高下字体盒线断缝；
//! 4. **同样式文本 Run 批处理**：跳过 `WIDE_CHAR_SPACER`，将同行内 `(fg, bold, italic, underline, strikeout)`
//!    一致的连续字符拼进复用缓冲后一次 `fill_text`，仅在字体或填色真正变化时切换状态；
//! 5. **多 Viewer 非定尺寸缩放与跟随（ADR 0022）**：`!owner` 时按生效网格建图，放不下先等比缩至最低 `0.5x`，
//!    仍超宽超高时平移跟随光标；网格小于窗口时外侧露 `ui.app` 窗口底色。

use falcon_term::alacritty_terminal::term::cell::Flags;
use falcon_term::alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};
use falcon_term::{SnapSelection, Snapshot};
use falcon_theme::{ResolvedTheme, Rgb as ThemeRgb};
use nsh_core::canvas::{CanvasTextAlign, CanvasTextBaseline};
use nsh_core::prelude::{Bounds, Hsla, Painter, Point, Points, Rgba, Size};

use crate::fonts::FONT_FAMILY_MONO;
use crate::theme::rgb_to_hsla;
use crate::zoom::pt_px;

/// 单扇终端画布的几何输入与生效视口变换（ADR 0022）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalViewport {
    pub pane_width: f32,
    pub pane_height: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub font_size: f32,
    pub is_owner: bool,
    pub focused: bool,
    pub scale: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl TerminalViewport {
    /// 由面板像素尺寸、基础字格尺寸与当前快照计算视口缩放与光标跟随偏移（ADR 0022）。
    pub fn compute(
        pane_width: f32,
        pane_height: f32,
        cell_width: f32,
        cell_height: f32,
        font_size: f32,
        is_owner: bool,
        focused: bool,
        snap: &Snapshot,
    ) -> Self {
        let pane_w = pane_width.max(16.0);
        let pane_h = pane_height.max(16.0);
        let cw = cell_width.max(4.0);
        let ch = cell_height.max(8.0);

        if is_owner {
            return Self {
                pane_width: pane_w,
                pane_height: pane_h,
                cell_width: cw,
                cell_height: ch,
                font_size,
                is_owner: true,
                focused,
                scale: 1.0,
                pan_x: 0.0,
                pan_y: 0.0,
            };
        }

        let grid_w = snap.size.cols as f32 * cw;
        let grid_h = snap.size.rows as f32 * ch;
        let fit = (pane_w / grid_w.max(1.0)).min(pane_h / grid_h.max(1.0));
        let scale = if fit >= 1.0 { 1.0 } else { fit.max(0.5) };

        let scaled_w = grid_w * scale;
        let scaled_h = grid_h * scale;
        let mut pan_x = 0.0;
        let mut pan_y = 0.0;

        if let Some(cur) = snap.cursor {
            if scaled_w > pane_w {
                let cx = (cur.col as f32 + 0.5) * cw * scale;
                pan_x = (cx - pane_w * 0.5).clamp(0.0, scaled_w - pane_w);
            }
            if scaled_h > pane_h {
                let cy = (cur.row as f32 + 0.5) * ch * scale;
                pan_y = (cy - pane_h * 0.5).clamp(0.0, scaled_h - pane_h);
            }
        }

        Self {
            pane_width: pane_w,
            pane_height: pane_h,
            cell_width: cw,
            cell_height: ch,
            font_size,
            is_owner: false,
            focused,
            scale,
            pan_x,
            pan_y,
        }
    }

    /// 面板内逻辑坐标 → 网格 `(col, row)`（0 起，夹在当前网格范围内）。
    pub fn pixel_to_cell(&self, x: f32, y: f32, cols: usize, rows: usize) -> (usize, usize) {
        let gx = (x + self.pan_x) / self.scale.max(0.1);
        let gy = (y + self.pan_y) / self.scale.max(0.1);
        let col = (gx / self.cell_width.max(1.0)).floor().max(0.0) as usize;
        let row = (gy / self.cell_height.max(1.0)).floor().max(0.0) as usize;
        (
            col.min(cols.saturating_sub(1)),
            row.min(rows.saturating_sub(1)),
        )
    }

    /// 光标锚点在面板内的像素矩形（供隐藏 `input()` IME 候选窗精确定位）。
    pub fn cursor_anchor_px(&self, snap: &Snapshot) -> (f32, f32) {
        if let Some(cur) = snap.cursor {
            let x = (cur.col as f32 * self.cell_width * self.scale - self.pan_x)
                .clamp(0.0, (self.pane_width - 2.0).max(0.0));
            let y = ((cur.row as f32 + 1.0) * self.cell_height * self.scale - self.pan_y)
                .clamp(0.0, (self.pane_height - 2.0).max(0.0));
            (x, y)
        } else {
            (4.0, self.cell_height.min(self.pane_height))
        }
    }
}

/// 预计算的 256 色 + 语义终端色表，避免在 `rows × cols` 热循环里重复转换。
struct FastPalette {
    indexed: [Hsla; 256],
    default_fg: Hsla,
    default_bg: Hsla,
    dim_fg: Hsla,
    cursor_bg: Hsla,
    cursor_fg: Hsla,
    selection_bg: Hsla,
    selection_fg: Option<Hsla>,
    app_bg: Hsla,
}

impl FastPalette {
    fn build(snap: &Snapshot, resolved: &ResolvedTheme) -> Self {
        let term = &resolved.terminal;
        let mut indexed = [Hsla::default(); 256];

        // 0..16：ANSI 标准与高亮色（优先采用程序 OSC 4 覆盖值）
        for i in 0..16 {
            indexed[i] = if let Some(ov) = snap.colors[i] {
                alac_rgb_to_hsla(ov.r, ov.g, ov.b)
            } else {
                rgb_to_hsla(term.ansi[i])
            };
        }

        static XTERM_256_TABLE: std::sync::LazyLock<[Hsla; 240]> =
            std::sync::LazyLock::new(|| std::array::from_fn(|i| xterm_256_color((i + 16) as u8)));

        // 16..256：优先 OSC 4，再看主题扩展表，最后直接查预计算的 xterm 256 色表
        for idx in 16..256 {
            indexed[idx] = if let Some(ov) = snap.colors[idx] {
                alac_rgb_to_hsla(ov.r, ov.g, ov.b)
            } else if let Some(ext) = term.extended_ansi.as_ref().and_then(|v| v.get(idx - 16)) {
                rgb_to_hsla(*ext)
            } else {
                XTERM_256_TABLE[idx - 16]
            };
        }

        let default_fg = snap.colors[NamedColor::Foreground]
            .map(|c| alac_rgb_to_hsla(c.r, c.g, c.b))
            .unwrap_or_else(|| rgb_to_hsla(term.foreground));
        let default_bg = snap.colors[NamedColor::Background]
            .map(|c| alac_rgb_to_hsla(c.r, c.g, c.b))
            .unwrap_or_else(|| rgb_to_hsla(term.background));
        let cursor_bg = snap.colors[NamedColor::Cursor]
            .map(|c| alac_rgb_to_hsla(c.r, c.g, c.b))
            .unwrap_or_else(|| rgb_to_hsla(term.cursor));

        Self {
            indexed,
            default_fg,
            default_bg,
            dim_fg: dim_hsla(default_fg),
            cursor_bg,
            cursor_fg: rgb_to_hsla(term.cursor_accent),
            selection_bg: rgb_to_hsla(term.selection_background),
            selection_fg: term.selection_foreground.map(rgb_to_hsla),
            app_bg: rgb_to_hsla(resolved.ui.app),
        }
    }

    #[inline]
    fn resolve_fg(&self, color: AnsiColor, flags: Flags) -> Hsla {
        let dim = flags.contains(Flags::DIM);
        let bold = flags.contains(Flags::BOLD);
        match color {
            AnsiColor::Spec(rgb) => {
                let c = alac_rgb_to_hsla(rgb.r, rgb.g, rgb.b);
                if dim { dim_hsla(c) } else { c }
            }
            AnsiColor::Indexed(idx) => {
                let actual = if bold && idx < 8 {
                    (idx + 8) as usize
                } else {
                    idx as usize
                };
                let c = self.indexed[actual];
                if dim { dim_hsla(c) } else { c }
            }
            AnsiColor::Named(named) => match named {
                NamedColor::Foreground | NamedColor::BrightForeground => {
                    if dim {
                        self.dim_fg
                    } else {
                        self.default_fg
                    }
                }
                NamedColor::DimForeground => self.dim_fg,
                NamedColor::Background => self.default_bg,
                NamedColor::Cursor => self.cursor_bg,
                _ => {
                    let n = named as usize;
                    if n < 16 {
                        let actual = if bold && n < 8 { n + 8 } else { n };
                        let c = self.indexed[actual];
                        if dim { dim_hsla(c) } else { c }
                    } else if n >= NamedColor::DimBlack as usize
                        && n <= NamedColor::DimWhite as usize
                    {
                        dim_hsla(self.indexed[n - NamedColor::DimBlack as usize])
                    } else {
                        self.default_fg
                    }
                }
            },
        }
    }

    #[inline]
    fn resolve_bg(&self, color: AnsiColor) -> Hsla {
        match color {
            AnsiColor::Spec(rgb) => alac_rgb_to_hsla(rgb.r, rgb.g, rgb.b),
            AnsiColor::Indexed(idx) => self.indexed[idx as usize],
            AnsiColor::Named(named) => match named {
                NamedColor::Background => self.default_bg,
                NamedColor::Foreground | NamedColor::BrightForeground => self.default_fg,
                NamedColor::DimForeground => self.dim_fg,
                NamedColor::Cursor => self.cursor_bg,
                _ => {
                    let n = named as usize;
                    if n < 16 {
                        self.indexed[n]
                    } else if n >= NamedColor::DimBlack as usize
                        && n <= NamedColor::DimWhite as usize
                    {
                        dim_hsla(self.indexed[n - NamedColor::DimBlack as usize])
                    } else {
                        self.default_bg
                    }
                }
            },
        }
    }
}

#[inline]
fn alac_rgb_to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    rgb_to_hsla(ThemeRgb { r, g, b })
}

#[inline]
fn dim_hsla(mut c: Hsla) -> Hsla {
    c.a *= 0.68;
    c
}

fn xterm_256_color(idx: u8) -> Hsla {
    if idx < 232 {
        let i = idx - 16;
        let r = i / 36;
        let g = (i % 36) / 6;
        let b = i % 6;
        let chan = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
        alac_rgb_to_hsla(chan(r), chan(g), chan(b))
    } else {
        let v = 8 + (idx - 232) * 10;
        alac_rgb_to_hsla(v, v, v)
    }
}

#[inline]
fn in_selection(sel: Option<SnapSelection>, row: usize, col: usize) -> bool {
    let Some(s) = sel else {
        return false;
    };
    let ((sr, sc), (er, ec)) = if s.start <= s.end {
        (s.start, s.end)
    } else {
        (s.end, s.start)
    };
    if row < sr || row > er {
        return false;
    }
    if s.is_block {
        let min_c = sc.min(ec);
        let max_c = sc.max(ec);
        col >= min_c && col <= max_c
    } else if sr == er {
        col >= sc && col <= ec
    } else if row == sr {
        col >= sc
    } else if row == er {
        col <= ec
    } else {
        true
    }
}

#[derive(Clone, Copy, PartialEq)]
struct RunStyle {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
}

/// 将一帧 `Snapshot` 录制成可由 NSH 各平台原生重放的 `Painter`。
pub fn paint_snapshot(
    snap: &Snapshot,
    resolved: &ResolvedTheme,
    vp: TerminalViewport,
    ime_preedit: Option<&str>,
) -> Painter {
    let pal = FastPalette::build(snap, resolved);
    let cols = snap.size.cols;
    let rows = snap.size.rows;
    let cw = vp.cell_width;
    let ch = vp.cell_height;
    let grid_w = cols as f32 * cw;
    let grid_h = rows as f32 * ch;

    let mut painter = Painter::new();
    let mut last_fill: Option<Hsla> = None;

    let set_fill = |p: Painter, last: &mut Option<Hsla>, c: Hsla| -> Painter {
        if *last == Some(c) {
            p
        } else {
            *last = Some(c);
            p.fill_color(c)
        }
    };

    // 非 owner 网格比窗口小时，整块底层填 `ui.app` 露出窗口底色（ADR 0022）
    let scaled_w = grid_w * vp.scale;
    let scaled_h = grid_h * vp.scale;
    if !vp.is_owner && (scaled_w < vp.pane_width || scaled_h < vp.pane_height) {
        painter = set_fill(painter, &mut last_fill, pal.app_bg);
        painter = painter.fill_rect(0.0, 0.0, vp.pane_width, vp.pane_height);
        painter = set_fill(painter, &mut last_fill, pal.default_bg);
        painter = painter.save();
        if (vp.pan_x.abs() > 0.01) || (vp.pan_y.abs() > 0.01) {
            painter = painter.translate(-vp.pan_x, -vp.pan_y);
        }
        if (vp.scale - 1.0).abs() > 0.001 {
            painter = painter.scale(vp.scale, vp.scale);
        }
        painter = painter.fill_rect(0.0, 0.0, grid_w, grid_h);
    } else {
        painter = set_fill(painter, &mut last_fill, pal.default_bg);
        painter = painter.fill_rect(0.0, 0.0, vp.pane_width, vp.pane_height);
        painter = painter.save();
        if (vp.pan_x.abs() > 0.01) || (vp.pan_y.abs() > 0.01) {
            painter = painter.translate(-vp.pan_x, -vp.pan_y);
        }
        if (vp.scale - 1.0).abs() > 0.001 {
            painter = painter.scale(vp.scale, vp.scale);
        }
    }

    // 第 1 遍：按行合并连续同色背景块（与 default_bg 一致的跳过）
    let mut cached_bg_spec: Option<(AnsiColor, Hsla)> = None;
    for row in 0..rows {
        let row_y = row as f32 * ch;
        let row_base = row * cols;
        let mut run_start = 0usize;
        let mut run_color: Option<Hsla> = None;

        for col in 0..cols {
            let cell = &snap.cells[row_base + col];
            let selected = in_selection(snap.selection, row, col);
            let bg = if selected {
                pal.selection_bg
            } else if cell.flags.contains(Flags::INVERSE) {
                pal.resolve_fg(cell.fg, cell.flags)
            } else if cell.bg == AnsiColor::Named(NamedColor::Background) {
                pal.default_bg
            } else if let Some((prev_c, prev_h)) = cached_bg_spec
                && prev_c == cell.bg
            {
                prev_h
            } else {
                let resolved_bg = pal.resolve_bg(cell.bg);
                cached_bg_spec = Some((cell.bg, resolved_bg));
                resolved_bg
            };
            let paint_bg = (bg != pal.default_bg).then_some(bg);
            if paint_bg != run_color {
                if let Some(c) = run_color {
                    let x = run_start as f32 * cw;
                    let w = (col - run_start) as f32 * cw;
                    painter = set_fill(painter, &mut last_fill, c);
                    painter = painter.fill_rect(x, row_y, w, ch);
                }
                run_start = col;
                run_color = paint_bg;
            }
        }
        if let Some(c) = run_color {
            let x = run_start as f32 * cw;
            let w = (cols - run_start) as f32 * cw;
            painter = set_fill(painter, &mut last_fill, c);
            painter = painter.fill_rect(x, row_y, w, ch);
        }
    }

    // 预构造 4 档 CSS font 字符串
    let fs = vp.font_size.max(8.0);
    let font_regular = format!("400 {fs:.1}px \"{FONT_FAMILY_MONO}\"");
    let font_bold = format!("700 {fs:.1}px \"{FONT_FAMILY_MONO}\"");
    let font_italic = format!("italic 400 {fs:.1}px \"{FONT_FAMILY_MONO}\"");
    let font_bold_italic = format!("italic 700 {fs:.1}px \"{FONT_FAMILY_MONO}\"");

    painter = painter
        .text_baseline(CanvasTextBaseline::Top)
        .text_align(CanvasTextAlign::Left)
        .font(&font_regular);
    let mut current_font: (bool, bool) = (false, false);

    // 垂直居中偏移，让字符在行高内光学对齐
    let text_pad_y = ((ch - fs) * 0.5).max(0.0);
    let mut run_buf = String::with_capacity(cols * 4);
    let mut cached_fg_spec: Option<(AnsiColor, Flags, Hsla)> = None;

    // 第 2 遍：盒线矢量几何 + 同样式文本 Run 批处理
    for row in 0..rows {
        let row_y = row as f32 * ch;
        let row_base = row * cols;
        let mut active_style: Option<RunStyle> = None;
        let mut run_col_start = 0usize;
        let mut run_col_end = 0usize;
        run_buf.clear();

        let flush_run = |mut p: Painter,
                         last_fill: &mut Option<Hsla>,
                         cur_font: &mut (bool, bool),
                         style: RunStyle,
                         start_col: usize,
                         end_col: usize,
                         text_str: &str|
         -> Painter {
            if text_str.is_empty() {
                return p;
            }
            let want_font = (style.bold, style.italic);
            if *cur_font != want_font {
                *cur_font = want_font;
                let spec = match want_font {
                    (false, false) => &font_regular,
                    (true, false) => &font_bold,
                    (false, true) => &font_italic,
                    (true, true) => &font_bold_italic,
                };
                p = p.font(spec);
            }
            p = set_fill(p, last_fill, style.fg);
            let x = start_col as f32 * cw;
            p = p.fill_text(text_str, x, row_y + text_pad_y);
            let w = (end_col.saturating_sub(start_col)) as f32 * cw;
            if style.underline && w > 0.0 {
                p = p.fill_rect(x, row_y + ch - 1.5, w, 1.0);
            }
            if style.strikeout && w > 0.0 {
                p = p.fill_rect(x, row_y + ch * 0.5, w, 1.0);
            }
            p
        };

        for col in 0..cols {
            let cell = &snap.cells[row_base + col];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            let c = cell.c;
            let cell_span = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };

            // 空白且无下划线/删除线：直接冲刷前序 Run，避免画全行空格
            let has_line_decor = cell
                .flags
                .intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE | Flags::STRIKEOUT);
            if (c == ' ' || c == '\0' || cell.flags.contains(Flags::HIDDEN)) && !has_line_decor {
                if let Some(st) = active_style.take() {
                    painter = flush_run(
                        painter,
                        &mut last_fill,
                        &mut current_font,
                        st,
                        run_col_start,
                        run_col_end,
                        &run_buf,
                    );
                    run_buf.clear();
                }
                continue;
            }

            let selected = in_selection(snap.selection, row, col);
            let fg_flags = cell.flags & (Flags::DIM | Flags::BOLD);
            let fg = if selected {
                pal.selection_fg
                    .unwrap_or_else(|| pal.resolve_fg(cell.fg, cell.flags))
            } else if cell.flags.contains(Flags::INVERSE) {
                pal.resolve_bg(cell.bg)
            } else if let Some((prev_c, prev_f, prev_h)) = cached_fg_spec
                && prev_c == cell.fg
                && prev_f == fg_flags
            {
                prev_h
            } else {
                let resolved_fg = pal.resolve_fg(cell.fg, cell.flags);
                cached_fg_spec = Some((cell.fg, fg_flags, resolved_fg));
                resolved_fg
            };

            // 盒线与块字符（U+2500..=U+259F）：先冲刷文本 Run，再按值传入 Painter 零克隆直绘矢量矩形
            if ('\u{2500}'..='\u{259F}').contains(&c) {
                if let Some(st) = active_style.take() {
                    painter = flush_run(
                        painter,
                        &mut last_fill,
                        &mut current_font,
                        st,
                        run_col_start,
                        run_col_end,
                        &run_buf,
                    );
                    run_buf.clear();
                }
                let (next_p, drawn) =
                    draw_box_or_block_glyph(painter, &mut last_fill, c, fg, col as f32 * cw, row_y, cw, ch);
                painter = next_p;
                if drawn {
                    continue;
                }
            }

            let style = RunStyle {
                fg,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell
                    .flags
                    .intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE),
                strikeout: cell.flags.contains(Flags::STRIKEOUT),
            };

            if active_style == Some(style) && run_col_end == col {
                run_buf.push(c);
                if let Some(zerowidth) = cell.zerowidth() {
                    for zw in zerowidth {
                        run_buf.push(*zw);
                    }
                }
                run_col_end = col + cell_span;
            } else {
                if let Some(prev) = active_style.take() {
                    painter = flush_run(
                        painter,
                        &mut last_fill,
                        &mut current_font,
                        prev,
                        run_col_start,
                        run_col_end,
                        &run_buf,
                    );
                    run_buf.clear();
                }
                active_style = Some(style);
                run_col_start = col;
                run_col_end = col + cell_span;
                run_buf.push(c);
                if let Some(zerowidth) = cell.zerowidth() {
                    for zw in zerowidth {
                        run_buf.push(*zw);
                    }
                }
            }
        }

        if let Some(st) = active_style.take() {
            painter = flush_run(
                painter,
                &mut last_fill,
                &mut current_font,
                st,
                run_col_start,
                run_col_end,
                &run_buf,
            );
        }
    }

    // 第 3 遍：光标与 IME 预编辑浮层
    if let Some(cur) = snap.cursor {
        let cur_w = if cur.wide { cw * 2.0 } else { cw };
        let cx = cur.col as f32 * cw;
        let cy = cur.row as f32 * ch;
        let shape = if !vp.focused {
            CursorShape::HollowBlock
        } else {
            cur.shape
        };
        match shape {
            CursorShape::Block => {
                painter = set_fill(painter, &mut last_fill, pal.cursor_bg);
                painter = painter.fill_rect(cx, cy, cur_w, ch);
                let under = snap.cell(cur.row, cur.col);
                if under.c != ' ' && under.c != '\0' {
                    let mut s = String::with_capacity(4);
                    s.push(under.c);
                    painter = set_fill(painter, &mut last_fill, pal.cursor_fg);
                    painter = painter.fill_text(&s, cx, cy + text_pad_y);
                }
            }
            CursorShape::Beam => {
                painter = set_fill(painter, &mut last_fill, pal.cursor_bg);
                painter = painter.fill_rect(cx, cy, 2.0, ch);
            }
            CursorShape::Underline => {
                painter = set_fill(painter, &mut last_fill, pal.cursor_bg);
                painter = painter.fill_rect(cx, cy + ch - 2.0, cur_w, 2.0);
            }
            CursorShape::HollowBlock => {
                painter = painter
                    .stroke_color(pal.cursor_bg)
                    .line_width(pt_px(1.0))
                    .stroke_rect(Bounds {
                        origin: Point {
                            x: Points(cx + 0.5),
                            y: Points(cy + 0.5),
                        },
                        size: Size {
                            width: Points((cur_w - 1.0).max(1.0)),
                            height: Points((ch - 1.0).max(1.0)),
                        },
                    });
            }
            CursorShape::Hidden => {}
        }

        if let Some(preedit) = ime_preedit.filter(|s| !s.is_empty()) {
            let cells_wide = unicode_width::UnicodeWidthStr::width(preedit).max(1) as f32;
            let pw = cells_wide * cw;
            painter = set_fill(painter, &mut last_fill, pal.default_bg);
            painter = painter.fill_rect(cx, cy, pw, ch);
            painter = set_fill(painter, &mut last_fill, pal.cursor_bg);
            painter = painter.fill_rect(cx, cy + ch - 2.0, pw, 2.0);
            painter = set_fill(painter, &mut last_fill, pal.default_fg);
            painter = painter.fill_text(preedit, cx, cy + text_pad_y);
        }
    }

    painter.restore()
}

/// 矢量绘制常见 Box Drawing（`U+2500..=U+257F`）与 Block Elements（`U+2580..=U+259F`）。
/// 未覆盖的冷门码点原样归还 `(p, false)`，零拷贝回退到 Ioskeley / TX-02 字形渲染。
fn draw_box_or_block_glyph(
    mut p: Painter,
    last_fill: &mut Option<Hsla>,
    c: char,
    fg: Hsla,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> (Painter, bool) {
    let ensure_fg = |p: Painter, last: &mut Option<Hsla>, color: Hsla| -> Painter {
        if *last == Some(color) {
            p
        } else {
            *last = Some(color);
            p.fill_color(color)
        }
    };

    // 阴影块元素（░ ▒ ▓）带透明度单画，其它共用前景色
    match c {
        '█' => {
            p = ensure_fg(p, last_fill, fg);
            return (p.fill_rect(x, y, w, h), true);
        }
        '▀' => {
            p = ensure_fg(p, last_fill, fg);
            return (p.fill_rect(x, y, w, h * 0.5), true);
        }
        '▄' => {
            p = ensure_fg(p, last_fill, fg);
            return (p.fill_rect(x, y + h * 0.5, w, h * 0.5), true);
        }
        '▌' => {
            p = ensure_fg(p, last_fill, fg);
            return (p.fill_rect(x, y, w * 0.5, h), true);
        }
        '▐' => {
            p = ensure_fg(p, last_fill, fg);
            return (p.fill_rect(x + w * 0.5, y, w * 0.5, h), true);
        }
        '░' | '▒' | '▓' => {
            let alpha = match c {
                '░' => 0.25,
                '▒' => 0.50,
                _ => 0.75,
            };
            let mut shaded: Rgba = fg.into();
            shaded.a *= alpha;
            let hc: Hsla = shaded.into();
            p = ensure_fg(p, last_fill, hc);
            return (p.fill_rect(x, y, w, h), true);
        }
        _ => {}
    }

    // (left, right, up, down, stroke_thickness)
    let (l, r, u, d, t): (bool, bool, bool, bool, f32) = match c {
        '─' | '╌' | '┄' => (true, true, false, false, 1.0),
        '━' | '═' => (true, true, false, false, 2.0),
        '│' | '╎' | '┆' => (false, false, true, true, 1.0),
        '┃' | '║' => (false, false, true, true, 2.0),
        '┌' | '╭' => (false, true, false, true, 1.0),
        '┏' | '╔' => (false, true, false, true, 2.0),
        '┐' | '╮' => (true, false, false, true, 1.0),
        '┓' | '╗' => (true, false, false, true, 2.0),
        '└' | '╰' => (false, true, true, false, 1.0),
        '┗' | '╚' => (false, true, true, false, 2.0),
        '┘' | '╯' => (true, false, true, false, 1.0),
        '┛' | '╝' => (true, false, true, false, 2.0),
        '├' => (false, true, true, true, 1.0),
        '┣' | '╠' => (false, true, true, true, 2.0),
        '┤' => (true, false, true, true, 1.0),
        '┫' | '╣' => (true, false, true, true, 2.0),
        '┬' => (true, true, false, true, 1.0),
        '┳' | '╦' => (true, true, false, true, 2.0),
        '┴' => (true, true, true, false, 1.0),
        '┻' | '╩' => (true, true, true, false, 2.0),
        '┼' => (true, true, true, true, 1.0),
        '╋' | '╬' => (true, true, true, true, 2.0),
        _ => return (p, false),
    };

    p = ensure_fg(p, last_fill, fg);
    let mid_x = (x + (w - t) * 0.5).floor();
    let mid_y = (y + (h - t) * 0.5).floor();

    if l && r {
        p = p.fill_rect(x, mid_y, w, t);
    } else if l {
        p = p.fill_rect(x, mid_y, (mid_x - x) + t, t);
    } else if r {
        p = p.fill_rect(mid_x, mid_y, (x + w) - mid_x, t);
    }

    if u && d {
        p = p.fill_rect(mid_x, y, t, h);
    } else if u {
        p = p.fill_rect(mid_x, y, t, (mid_y - y) + t);
    } else if d {
        p = p.fill_rect(mid_x, mid_y, t, (y + h) - mid_y);
    }

    (p, true)
}

/// 供 `Idiom::Terminal`（TUI 宿主）使用的纯字符网格投影：跳过宽字符占位格，输出 `max_rows` 行文本。
pub fn render_tui_lines(snap: &Snapshot, max_cols: usize, max_rows: usize) -> Vec<String> {
    let rows = snap.size.rows.min(max_rows.max(1));
    let cols = snap.size.cols.min(max_cols.max(2));
    let mut lines = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut line = String::with_capacity(cols);
        for c in 0..cols {
            let cell = snap.cell(r, c);
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            line.push(ch);
        }
        lines.push(line.trim_end().to_string());
    }
    lines
}
