//! 终端画面元素：把一份 [`falcon_term::Snapshot`] 排版绘制出来，并挂 IME 输入处理器。
//!
//! 以 Zed 的 `crates/terminal_view/src/terminal_element.rs`（zed-industries/zed@279fe07，
//! GPL-3.0-or-later，Copyright Zed Industries）为底本改写：`BatchedTextRun`（同样式相邻格合成
//! 一段 run，`shape_line` 带 force_width 排成等宽网格）、`LayoutRect` / 背景区合并、块元素字符
//! 自己画成矩形（`▀▄█░▒▓`、象限、六分块）、`TerminalInputHandler` 都来自那里。剥掉的：
//! workspace / settings / 搜索高亮 / 块下插件 / 路径跳转。
//!
//! 与 Zed 不同的口径（为了与 web 的 xterm.js 引擎画出同样的东西）：
//! - 行高 = 字体自然高度（ascent + descent）× 行高倍数，与 xterm 的 lineHeight 语义一致——
//!   web 默认倍数 1，不是 13px；
//! - 粗体 + ANSI 0–7 前景画成亮色（xterm 的 drawBoldTextInBrightColors 默认开）；
//! - 不做最小对比度调整（xterm 的 minimumContrastRatio 默认 1）；
//! - dim 用 0.5 透明度（xterm 的 DIM_OPACITY）；
//! - 失焦时块光标画成空心框（xterm 的 cursorInactiveStyle = outline）。

use std::mem;
use std::sync::Arc;

use falcon_term::Snapshot;
use falcon_term::alacritty_terminal::term::TermMode;
use falcon_term::alacritty_terminal::term::cell::{Cell, Flags};
use falcon_term::alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use gpui_kit::{
    App, Bounds, ContentMask, Element, ElementId, Entity, FocusHandle, Font, FontStyle,
    FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InputHandler, IntoElement,
    LayoutId, Pixels, Point, SharedString, StrikethroughStyle, Style, TextRun, UTF16Selection,
    UnderlineStyle, Window, fill, outline, point, px, relative, size,
};

use super::view::TerminalView;

/// 终端配色：主题派生的 16 色 + 前景 / 背景 / 光标 / 选区。
#[derive(Clone, Debug, PartialEq)]
pub struct TermPalette {
    pub foreground: Hsla,
    pub background: Hsla,
    pub cursor: Hsla,
    pub cursor_text: Hsla,
    pub selection_background: Hsla,
    /// 主题给了 selection-foreground 才有；否则选区里的字保持原色
    pub selection_foreground: Option<Hsla>,
    pub ansi: [Hsla; 16],
}

/// 排版参数：字体、字号、行高倍数。
#[derive(Clone, Debug, PartialEq)]
pub struct TermTextStyle {
    pub font: Font,
    pub font_size: Pixels,
    pub line_height_factor: f32,
}

/// 一次布局算出来的网格几何，视图用它把鼠标位置换算成格子。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalGeometry {
    /// 第一格左上角（窗口坐标，已按设备像素取整）
    pub origin: Point<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub cols: usize,
    pub rows: usize,
}

impl TerminalGeometry {
    /// 窗口坐标 → (行, 列)，夹到网格内；同时返回点落在格子的左半还是右半（选区用）。
    pub fn cell_at(&self, pos: Point<Pixels>) -> (usize, usize, bool) {
        let x = f32::from(pos.x - self.origin.x).max(0.0);
        let y = f32::from(pos.y - self.origin.y).max(0.0);
        let cw = f32::from(self.cell_width).max(1.0);
        let lh = f32::from(self.line_height).max(1.0);
        let col = ((x / cw) as usize).min(self.cols.saturating_sub(1));
        let row = ((y / lh) as usize).min(self.rows.saturating_sub(1));
        let right_half = (x / cw).fract() >= 0.5;
        (row, col, right_half)
    }

    /// 相对终端左上角的像素坐标（?1016 的鼠标上报用），夹到画布内。
    pub fn pixel_at(&self, pos: Point<Pixels>) -> (i32, i32) {
        let w = f32::from(self.cell_width) * self.cols as f32;
        let h = f32::from(self.line_height) * self.rows as f32;
        let x = f32::from(pos.x - self.origin.x).clamp(0.0, (w - 1.0).max(0.0));
        let y = f32::from(pos.y - self.origin.y).clamp(0.0, (h - 1.0).max(0.0));
        (x as i32, y as i32)
    }
}

/// 悬停中的链接在可见区里的范围（同一行，列含首不含尾）。
#[derive(Clone, Debug, PartialEq)]
pub struct HoveredLink {
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub url: String,
}

pub struct TerminalElement {
    view: Entity<TerminalView>,
    snapshot: Arc<Snapshot>,
    palette: TermPalette,
    style: TermTextStyle,
    focus: FocusHandle,
    focused: bool,
    cursor_visible: bool,
    marked_text: Option<String>,
    hovered_link: Option<HoveredLink>,
    /// 左 / 上的内边距（web 是 pl-2 py-1.5）
    padding: Point<Pixels>,
}

impl TerminalElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        view: Entity<TerminalView>,
        snapshot: Arc<Snapshot>,
        palette: TermPalette,
        style: TermTextStyle,
        focus: FocusHandle,
        focused: bool,
        cursor_visible: bool,
        marked_text: Option<String>,
        hovered_link: Option<HoveredLink>,
    ) -> Self {
        Self {
            view,
            snapshot,
            palette,
            style,
            focus,
            focused,
            cursor_visible,
            marked_text,
            hovered_link,
            padding: point(px(8.), px(6.)),
        }
    }
}

// ---------------------------------------------------------------------------
// 以下排版结构抄自 Zed terminal_element.rs（见文件头），坐标改成可见区的 (行, 列)。
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct LayoutPoint {
    line: i32,
    column: i32,
}

impl LayoutPoint {
    fn new(line: i32, column: i32) -> Self {
        Self { line, column }
    }
}

/// 同样式的相邻格合成的一段文字
#[derive(Debug)]
struct BatchedTextRun {
    start_point: LayoutPoint,
    text: String,
    cell_count: usize,
    style: TextRun,
}

impl BatchedTextRun {
    fn new_from_char(start_point: LayoutPoint, c: char, style: TextRun) -> Self {
        let mut text = String::with_capacity(100);
        text.push(c);
        BatchedTextRun {
            start_point,
            text,
            cell_count: 1,
            style,
        }
    }

    fn can_append(&self, other_style: &TextRun) -> bool {
        self.style.font == other_style.font
            && self.style.color == other_style.color
            && self.style.background_color == other_style.background_color
            && self.style.underline == other_style.underline
            && self.style.strikethrough == other_style.strikethrough
    }

    fn append_char(&mut self, c: char) {
        self.append_char_internal(c, true);
    }

    fn append_zero_width_chars(&mut self, chars: &[char]) {
        for &c in chars {
            self.append_char_internal(c, false);
        }
    }

    fn append_char_internal(&mut self, c: char, counts_cell: bool) {
        self.text.push(c);
        if counts_cell {
            self.cell_count += 1;
        }
        self.style.len += c.len_utf8();
    }

    fn paint(
        &self,
        origin: Point<Pixels>,
        geo: &TerminalGeometry,
        font_size: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) {
        let pos = point(
            origin.x + geo.cell_width * self.start_point.column as f32,
            origin.y + geo.line_height * self.start_point.line as f32,
        );
        let line = window.text_system().shape_line(
            SharedString::from(self.text.clone()),
            font_size,
            std::slice::from_ref(&self.style),
            Some(geo.cell_width),
        );
        if let Err(err) = line.paint(pos, geo.line_height, gpui_kit::TextAlign::Left, None, window, cx) {
            log::debug!("终端文字绘制失败：{err:#}");
        }
    }
}

/// 块元素在子格网格上画：每格横 8 份（八分之一块）、竖 24 份（八分块与六分块三等分的最小公倍数）。
const BLOCK_SUBCELL_COLUMNS: i32 = 8;
const BLOCK_SUBCELL_LINES: i32 = 24;

#[derive(Clone, Debug)]
struct BlockElementLayoutRect {
    point: LayoutPoint,
    num_of_columns: usize,
    num_of_lines: usize,
    color: Hsla,
}

impl BlockElementLayoutRect {
    fn paint(&self, origin: Point<Pixels>, geo: &TerminalGeometry, window: &mut Window) {
        let subcell_width = geo.cell_width / BLOCK_SUBCELL_COLUMNS as f32;
        let subcell_height = geo.line_height / BLOCK_SUBCELL_LINES as f32;
        let position = point(
            origin.x + subcell_width * self.point.column as f32,
            origin.y + subcell_height * self.point.line as f32,
        );
        let size = size(
            subcell_width * self.num_of_columns as f32,
            subcell_height * self.num_of_lines as f32,
        );
        window.paint_quad(fill(Bounds::new(position, size), self.color));
    }
}

#[derive(Clone, Debug, Default)]
struct LayoutRect {
    point: LayoutPoint,
    num_of_cells: usize,
    color: Hsla,
}

impl LayoutRect {
    fn paint(&self, origin: Point<Pixels>, geo: &TerminalGeometry, window: &mut Window) {
        let position = point(
            (origin.x + geo.cell_width * self.point.column as f32).floor(),
            origin.y + geo.line_height * self.point.line as f32,
        );
        let size = size(
            (geo.cell_width * self.num_of_cells as f32).ceil(),
            geo.line_height,
        );
        window.paint_quad(fill(Bounds::new(position, size), self.color));
    }
}

/// 逻辑网格上的一块同色矩形区域
#[derive(Debug, Clone)]
struct BackgroundRegion {
    start_line: i32,
    start_col: i32,
    end_line: i32,
    end_col: i32,
    color: Hsla,
}

impl BackgroundRegion {
    fn new(line: i32, col: i32, color: Hsla) -> Self {
        Self::with_extents(line, col, line, col, color)
    }

    fn with_extents(start_line: i32, start_col: i32, end_line: i32, end_col: i32, color: Hsla) -> Self {
        BackgroundRegion {
            start_line,
            start_col,
            end_line,
            end_col,
            color,
        }
    }

    fn can_merge_with(&self, other: &BackgroundRegion) -> bool {
        if self.color != other.color {
            return false;
        }
        if self.start_line == other.start_line && self.end_line == other.end_line {
            return self.end_col + 1 == other.start_col || other.end_col + 1 == self.start_col;
        }
        if self.start_col == other.start_col && self.end_col == other.end_col {
            return self.end_line + 1 == other.start_line || other.end_line + 1 == self.start_line;
        }
        false
    }

    fn merge_with(&mut self, other: &BackgroundRegion) {
        self.start_line = self.start_line.min(other.start_line);
        self.start_col = self.start_col.min(other.start_col);
        self.end_line = self.end_line.max(other.end_line);
        self.end_col = self.end_col.max(other.end_col);
    }
}

fn merge_background_regions(regions: Vec<BackgroundRegion>) -> Vec<BackgroundRegion> {
    let mut merged = regions;
    let mut changed = true;
    while changed {
        changed = false;
        let mut i = 0;
        while i < merged.len() {
            let mut j = i + 1;
            while j < merged.len() {
                if merged[i].can_merge_with(&merged[j]) {
                    let other = merged.remove(j);
                    merged[i].merge_with(&other);
                    changed = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }
    merged
}

fn sextant_char_to_filled_bits(ch: char) -> Option<u8> {
    let offset = (ch as u32).checked_sub(0x1FB00)?;
    if offset > 0x3B {
        return None;
    }
    Some((offset + 1 + u32::from(offset >= 20) + u32::from(offset >= 40)) as u8)
}

fn quadrant_char_to_filled_bits(ch: char) -> Option<u8> {
    Some(match ch {
        '▘' => 0b0001,
        '▝' => 0b0010,
        '▖' => 0b0100,
        '▗' => 0b1000,
        '▚' => 0b1001,
        '▞' => 0b0110,
        '▛' => 0b0111,
        '▜' => 0b1011,
        '▙' => 0b1101,
        '▟' => 0b1110,
        _ => return None,
    })
}

/// `(column, line, num_of_columns, num_of_lines)`，子格单位
fn block_char_to_rect(ch: char) -> Option<(i32, i32, i32, i32)> {
    let codepoint = ch as u32;
    Some(match codepoint {
        0x2580 => (0, 0, 8, 12),
        0x2581..=0x2588 => {
            let eighths = (codepoint - 0x2580) as i32;
            (0, 24 - eighths * 3, 8, eighths * 3)
        }
        0x2589..=0x258F => (0, 0, (0x2590 - codepoint) as i32, 24),
        0x2590 => (4, 0, 4, 24),
        0x2594 => (0, 0, 8, 3),
        0x2595 => (7, 0, 1, 24),
        _ => return None,
    })
}

fn shade_char_to_opacity(ch: char) -> Option<f32> {
    match ch {
        '░' => Some(0.25),
        '▒' => Some(0.5),
        '▓' => Some(0.75),
        _ => None,
    }
}

fn push_block_element_region(
    p: LayoutPoint,
    column: i32,
    line: i32,
    num_of_columns: i32,
    num_of_lines: i32,
    color: Hsla,
    regions: &mut Vec<BackgroundRegion>,
) {
    let start_line = p.line * BLOCK_SUBCELL_LINES + line;
    let start_col = p.column * BLOCK_SUBCELL_COLUMNS + column;
    let end_line = start_line + num_of_lines - 1;
    let end_col = start_col + num_of_columns - 1;
    // 连续的 `█`（QR 码之类）先就地延长，让后面的二次合并输入保持很小
    if let Some(last) = regions.last_mut()
        && last.color == color
        && last.start_line == start_line
        && last.end_line == end_line
        && last.end_col + 1 == start_col
    {
        last.end_col = end_col;
        return;
    }
    regions.push(BackgroundRegion::with_extents(start_line, start_col, end_line, end_col, color));
}

fn collect_block_element_regions(p: LayoutPoint, ch: char, color: Hsla, regions: &mut Vec<BackgroundRegion>) -> bool {
    if let Some((column, line, w, h)) = block_char_to_rect(ch) {
        push_block_element_region(p, column, line, w, h, color, regions);
        return true;
    }
    if let Some(filled) = quadrant_char_to_filled_bits(ch) {
        for row in 0..2 {
            for column in 0..2 {
                if filled & (1 << (row * 2 + column)) != 0 {
                    push_block_element_region(p, column * 4, row * 12, 4, 12, color, regions);
                }
            }
        }
        return true;
    }
    if let Some(filled) = sextant_char_to_filled_bits(ch) {
        for row in 0..3 {
            for column in 0..2 {
                if filled & (1 << (row * 2 + column)) != 0 {
                    push_block_element_region(p, column * 4, row * 8, 4, 8, color, regions);
                }
            }
        }
        return true;
    }
    if let Some(opacity) = shade_char_to_opacity(ch) {
        push_block_element_region(p, 0, 0, 8, 24, color.opacity(opacity), regions);
        return true;
    }
    false
}

fn block_element_regions_to_rects(regions: Vec<BackgroundRegion>) -> Vec<BlockElementLayoutRect> {
    merge_background_regions(regions)
        .into_iter()
        .map(|r| BlockElementLayoutRect {
            point: LayoutPoint::new(r.start_line, r.start_col),
            num_of_columns: (r.end_col - r.start_col + 1) as usize,
            num_of_lines: (r.end_line - r.start_line + 1) as usize,
            color: r.color,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 颜色
// ---------------------------------------------------------------------------

/// xterm 256 色表里 16–255 的部分：6×6×6 色立方 + 24 级灰。
fn indexed_rgb(index: u8) -> (u8, u8, u8) {
    if index >= 232 {
        let v = 8 + (index - 232) * 10;
        return (v, v, v);
    }
    let i = index - 16;
    let step = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
    (step(i / 36), step((i / 6) % 6), step(i % 6))
}

fn rgb_to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    gpui_kit::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

struct Colorizer<'a> {
    palette: &'a TermPalette,
    snapshot: &'a Snapshot,
}

impl Colorizer<'_> {
    /// 程序用 OSC 4/10/11 改过的颜色优先，其次主题。
    fn overridden(&self, index: usize) -> Option<Hsla> {
        self.snapshot.colors[index].map(|c| rgb_to_hsla(c.r, c.g, c.b))
    }

    fn resolve(&self, color: &Color, is_fg: bool) -> Hsla {
        match color {
            Color::Spec(rgb) => rgb_to_hsla(rgb.r, rgb.g, rgb.b),
            Color::Indexed(i) => self.indexed(*i),
            Color::Named(named) => self.named(*named, is_fg),
        }
    }

    fn indexed(&self, i: u8) -> Hsla {
        if let Some(c) = self.overridden(i as usize) {
            return c;
        }
        if i < 16 {
            return self.palette.ansi[i as usize];
        }
        let (r, g, b) = indexed_rgb(i);
        rgb_to_hsla(r, g, b)
    }

    fn named(&self, named: NamedColor, is_fg: bool) -> Hsla {
        let index = named as usize;
        if let Some(c) = self.overridden(index) {
            return c;
        }
        match named {
            NamedColor::Foreground | NamedColor::BrightForeground => self.palette.foreground,
            NamedColor::DimForeground => self.palette.foreground.opacity(0.5),
            NamedColor::Background => self.palette.background,
            NamedColor::Cursor => self.palette.cursor,
            n if (n as usize) < 16 => self.palette.ansi[n as usize],
            // DimBlack..DimWhite：alacritty 在 dim 时换过来的色，xterm 口径是原色 + 透明度
            n => {
                let base = (n as usize).saturating_sub(NamedColor::DimBlack as usize) % 8;
                let c = self.palette.ansi[base];
                if is_fg { c.opacity(0.5) } else { c }
            }
        }
    }
}

fn is_default_background(color: &Color) -> bool {
    matches!(color, Color::Named(NamedColor::Background))
}

/// 空白格：空格、默认底色、没有可见的样式修饰——整格可以不画
fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && is_default_background(&cell.bg)
        && cell.hyperlink().is_none()
        && !cell.flags.intersects(
            Flags::INVERSE
                | Flags::ALL_UNDERLINES
                | Flags::STRIKEOUT,
        )
}

fn in_selection(sel: &falcon_term::SnapSelection, row: usize, col: usize, cols: usize) -> bool {
    let (sr, sc) = sel.start;
    let (er, ec) = sel.end;
    if row < sr || row > er {
        return false;
    }
    if sel.is_block {
        let (lo, hi) = (sc.min(ec), sc.max(ec));
        return col >= lo && col <= hi;
    }
    let from = if row == sr { sc } else { 0 };
    let to = if row == er { ec } else { cols.saturating_sub(1) };
    col >= from && col <= to
}

/// 本帧布局的产物，paint 直接用
pub struct LayoutState {
    hitbox: Hitbox,
    geometry: TerminalGeometry,
    rects: Vec<LayoutRect>,
    selection_rects: Vec<LayoutRect>,
    runs: Vec<BatchedTextRun>,
    block_rects: Vec<BlockElementLayoutRect>,
    cursor: Option<CursorPaint>,
    ime_cursor_bounds: Option<Bounds<Pixels>>,
    font_size: Pixels,
    ime_font: Font,
}

struct CursorPaint {
    bounds: Bounds<Pixels>,
    shape: CursorShape,
    color: Hsla,
    /// 块光标上反色画的那个字
    text: Option<BatchedTextRun>,
}

impl TerminalElement {
    fn layout_grid(&self, colors: &Colorizer<'_>) -> (Vec<LayoutRect>, Vec<LayoutRect>, Vec<BatchedTextRun>, Vec<BlockElementLayoutRect>) {
        let snap = &*self.snapshot;
        let cols = snap.size.cols;
        let mut runs: Vec<BatchedTextRun> = Vec::with_capacity(snap.size.rows * 4);
        let mut regions: Vec<BackgroundRegion> = Vec::new();
        let mut selection_regions: Vec<BackgroundRegion> = Vec::new();
        let mut block_regions: Vec<BackgroundRegion> = Vec::new();
        let mut current: Option<BatchedTextRun> = None;

        for row in 0..snap.size.rows {
            if let Some(batch) = current.take() {
                runs.push(batch);
            }
            let line = row as i32;
            let mut previous_cell_had_extras = false;
            for col in 0..cols {
                let cell = snap.cell(row, col);
                let selected = snap
                    .selection
                    .as_ref()
                    .is_some_and(|s| in_selection(s, row, col, cols));

                let mut fg = cell.fg;
                let mut bg = cell.bg;
                // xterm 的 drawBoldTextInBrightColors：粗体的 ANSI 0–7 前景换成对应亮色
                if cell.flags.contains(Flags::BOLD)
                    && let Color::Named(n) = fg
                    && (n as usize) < 8
                {
                    fg = Color::Indexed(n as u8 + 8);
                }
                if cell.flags.contains(Flags::INVERSE) {
                    mem::swap(&mut fg, &mut bg);
                }

                if selected {
                    let c = colors.palette.selection_background;
                    push_region(&mut selection_regions, line, col as i32, c);
                } else if !is_default_background(&bg) || cell.flags.contains(Flags::INVERSE) {
                    let c = colors.resolve(&bg, false);
                    push_region(&mut regions, line, col as i32, c);
                }

                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                // emoji 变体序列后面跟着的占位空格不单独画
                if cell.c == ' ' && previous_cell_had_extras {
                    previous_cell_had_extras = false;
                    continue;
                }
                let zerowidth = cell.zerowidth();
                previous_cell_had_extras = zerowidth.is_some_and(|z| !z.is_empty());

                if is_blank(cell) || cell.flags.contains(Flags::HIDDEN) {
                    continue;
                }

                let mut color = colors.resolve(&fg, true);
                if selected && let Some(sel_fg) = colors.palette.selection_foreground {
                    color = sel_fg;
                }
                if cell.flags.contains(Flags::DIM) {
                    color = color.opacity(0.5);
                }

                let p = LayoutPoint::new(line, col as i32);
                if collect_block_element_regions(p, cell.c, color, &mut block_regions) {
                    if let Some(batch) = current.take() {
                        runs.push(batch);
                    }
                    continue;
                }

                let hovered = self
                    .hovered_link
                    .as_ref()
                    .is_some_and(|l| l.row == row && col >= l.start_col && col < l.end_col);
                let style = self.cell_run_style(cell, color, hovered);

                match current.as_mut() {
                    Some(batch)
                        if batch.can_append(&style)
                            && batch.start_point.line == line
                            && batch.start_point.column + batch.cell_count as i32 == col as i32 =>
                    {
                        batch.append_char(cell.c);
                        if let Some(z) = zerowidth {
                            batch.append_zero_width_chars(z);
                        }
                    }
                    _ => {
                        if let Some(batch) = current.take() {
                            runs.push(batch);
                        }
                        let mut batch = BatchedTextRun::new_from_char(p, cell.c, style);
                        if let Some(z) = zerowidth {
                            batch.append_zero_width_chars(z);
                        }
                        current = Some(batch);
                    }
                }
            }
        }
        if let Some(batch) = current {
            runs.push(batch);
        }

        let to_rects = |regions: Vec<BackgroundRegion>| {
            let mut rects = Vec::new();
            for r in merge_background_regions(regions) {
                for line in r.start_line..=r.end_line {
                    rects.push(LayoutRect {
                        point: LayoutPoint::new(line, r.start_col),
                        num_of_cells: (r.end_col - r.start_col + 1) as usize,
                        color: r.color,
                    });
                }
            }
            rects
        };
        (
            to_rects(regions),
            to_rects(selection_regions),
            runs,
            block_element_regions_to_rects(block_regions),
        )
    }

    fn cell_run_style(&self, cell: &Cell, color: Hsla, hovered_link: bool) -> TextRun {
        let flags = cell.flags;
        let underline = (flags.intersects(Flags::ALL_UNDERLINES) || hovered_link).then(|| UnderlineStyle {
            color: Some(color),
            thickness: px(1.0),
            wavy: flags.contains(Flags::UNDERCURL),
        });
        let strikethrough = flags.contains(Flags::STRIKEOUT).then(|| StrikethroughStyle {
            color: Some(color),
            thickness: px(1.0),
        });
        TextRun {
            len: cell.c.len_utf8(),
            color,
            background_color: None,
            font: Font {
                weight: if flags.contains(Flags::BOLD) {
                    FontWeight::BOLD
                } else {
                    self.style.font.weight
                },
                style: if flags.contains(Flags::ITALIC) {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                },
                ..self.style.font.clone()
            },
            underline,
            strikethrough,
        }
    }
}

fn push_region(regions: &mut Vec<BackgroundRegion>, line: i32, col: i32, color: Hsla) {
    if let Some(last) = regions.last_mut()
        && last.color == color
        && last.start_line == line
        && last.end_line == line
        && last.end_col + 1 == col
    {
        last.end_col = col;
    } else {
        regions.push(BackgroundRegion::new(line, col, color));
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = LayoutState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&self.style.font);
        let font_size = self.style.font_size;
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map(|a| a.width)
            .unwrap_or(font_size * 0.6);
        // xterm 的 lineHeight 倍数乘的是字体自然高度（ascent + descent），不是字号
        let natural = text_system.ascent(font_id, font_size) + text_system.descent(font_id, font_size).abs();
        let scale = window.scale_factor().max(1.0);
        let line_height = {
            let lh = f32::from(natural) * self.style.line_height_factor;
            px((lh * scale).ceil().max(1.0) / scale)
        };

        let snap_px = |v: Pixels| px((f32::from(v) * scale).floor() / scale);
        let origin = point(
            snap_px(bounds.origin.x + self.padding.x),
            snap_px(bounds.origin.y + self.padding.y),
        );
        let avail_w = (bounds.size.width - self.padding.x).max(px(0.));
        let avail_h = (bounds.size.height - self.padding.y * 2.).max(px(0.));
        let fit_cols = (f32::from(avail_w) / f32::from(cell_width)).floor() as usize;
        let fit_rows = (f32::from(avail_h) / f32::from(line_height)).floor() as usize;
        let geometry = TerminalGeometry {
            origin,
            cell_width,
            line_height,
            cols: fit_cols.max(2),
            rows: fit_rows.max(1),
        };
        // 视图据此 resize 终端 / 通知服务端，并在鼠标事件里换算格子。
        // 量不出像样的格子（画布首帧还没拿到自己的 bounds，窗格按 0 排；列被挤没）时不报：
        // zellij 在 alt screen 里，没有回滚，缩到 2×3 再放回来整屏内容就丢了——回放先于
        // 首次测量到达时正是这样，服务端尺寸没变不会重画，终端就一直是空的。xterm 的 fit
        // addon 同样在量不出尺寸时保留旧值
        if fit_cols >= 2 && fit_rows >= 1 {
            self.view.update(cx, |view, cx| view.set_geometry(geometry, cx));
        }

        let colors = Colorizer {
            palette: &self.palette,
            snapshot: &self.snapshot,
        };
        let (rects, selection_rects, runs, block_rects) = self.layout_grid(&colors);

        let cursor = self.snapshot.cursor.and_then(|c| {
            if c.row >= self.snapshot.size.rows || c.col >= self.snapshot.size.cols {
                return None;
            }
            let width = if c.wide { cell_width * 2. } else { cell_width };
            let bounds = Bounds::new(
                point(cell_width * c.col as f32, line_height * c.row as f32),
                size(width, line_height),
            );
            let cell = self.snapshot.cell(c.row, c.col);
            let text = (c.shape == CursorShape::Block && self.focused && cell.c != ' ').then(|| {
                let mut style = self.cell_run_style(cell, self.palette.cursor_text, false);
                style.underline = None;
                let mut run = BatchedTextRun::new_from_char(LayoutPoint::new(c.row as i32, c.col as i32), cell.c, style);
                if let Some(z) = cell.zerowidth() {
                    run.append_zero_width_chars(z);
                }
                run
            });
            Some(CursorPaint {
                bounds,
                shape: c.shape,
                color: self.palette.cursor,
                text,
            })
        });
        let ime_cursor_bounds = self.snapshot.cursor.map(|c| {
            Bounds::new(
                point(cell_width * c.col as f32, line_height * c.row as f32),
                size(cell_width, line_height),
            )
        });

        LayoutState {
            hitbox,
            geometry,
            rects,
            selection_rects,
            runs,
            block_rects,
            cursor,
            ime_cursor_bounds,
            font_size,
            ime_font: self.style.font.clone(),
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, self.palette.background));
            let origin = layout.geometry.origin;
            let geo = layout.geometry;

            window.handle_input(
                &self.focus,
                TerminalInputHandler {
                    view: self.view.clone(),
                    cursor_bounds: layout.ime_cursor_bounds.map(|b| b + origin),
                    cell_width: geo.cell_width,
                },
                cx,
            );
            window.set_cursor_style(
                if self.hovered_link.is_some() {
                    gpui_kit::CursorStyle::PointingHand
                } else {
                    gpui_kit::CursorStyle::IBeam
                },
                &layout.hitbox,
            );

            for rect in &layout.rects {
                rect.paint(origin, &geo, window);
            }
            for rect in &layout.selection_rects {
                rect.paint(origin, &geo, window);
            }
            for run in &layout.runs {
                run.paint(origin, &geo, layout.font_size, window, cx);
            }
            for rect in &layout.block_rects {
                rect.paint(origin, &geo, window);
            }

            let marked = self.marked_text.as_ref().filter(|t| !t.is_empty());
            if let (Some(text), Some(ime_bounds)) = (marked, layout.ime_cursor_bounds) {
                // 组字串画在光标处、盖在格子上，不进 PTY（web 的 rio 引擎是 DOM 预编辑覆盖层）
                let pos = (ime_bounds + origin).origin;
                let run = TextRun {
                    len: text.len(),
                    font: layout.ime_font.clone(),
                    color: self.palette.foreground,
                    background_color: None,
                    underline: Some(UnderlineStyle {
                        color: Some(self.palette.foreground),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    strikethrough: None,
                };
                let shaped = window.text_system().shape_line(
                    SharedString::from(text.clone()),
                    layout.font_size,
                    &[run],
                    None,
                );
                window.paint_quad(fill(Bounds::new(pos, size(shaped.width, geo.line_height)), self.palette.background));
                let _ = shaped.paint(pos, geo.line_height, gpui_kit::TextAlign::Left, None, window, cx);
            } else if self.cursor_visible
                && let Some(cursor) = layout.cursor.as_ref()
            {
                paint_cursor(cursor, origin, self.focused, &geo, layout.font_size, window, cx);
            }
        });
    }
}

fn paint_cursor(
    cursor: &CursorPaint,
    origin: Point<Pixels>,
    focused: bool,
    geo: &TerminalGeometry,
    font_size: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let b = cursor.bounds + origin;
    if !focused {
        window.paint_quad(outline(b, cursor.color, gpui_kit::BorderStyle::Solid));
        return;
    }
    match cursor.shape {
        CursorShape::Block => {
            window.paint_quad(fill(b, cursor.color));
            if let Some(text) = &cursor.text {
                text.paint(origin, geo, font_size, window, cx);
            }
        }
        CursorShape::Beam => {
            window.paint_quad(fill(Bounds::new(b.origin, size(px(1.5), b.size.height)), cursor.color));
        }
        CursorShape::Underline => {
            let h = px(2.);
            window.paint_quad(fill(
                Bounds::new(point(b.origin.x, b.origin.y + b.size.height - h), size(b.size.width, h)),
                cursor.color,
            ));
        }
        CursorShape::HollowBlock => {
            window.paint_quad(outline(b, cursor.color, gpui_kit::BorderStyle::Solid));
        }
        CursorShape::Hidden => {}
    }
}

/// IME：抄自 Zed 的 TerminalInputHandler。组字串存在视图上，由本元素画在光标处；
/// 上屏的文本经视图发给会话。
struct TerminalInputHandler {
    view: Entity<TerminalView>,
    cursor_bounds: Option<Bounds<Pixels>>,
    cell_width: Pixels,
}

impl InputHandler for TerminalInputHandler {
    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut App) -> Option<UTF16Selection> {
        // alt screen（TUI 全屏）里也要给出一个合法选区，输入法才知道候选框放哪
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<std::ops::Range<usize>> {
        self.view.read(cx).marked_text_range()
    }

    fn text_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        None
    }

    fn replace_text_in_range(&mut self, _: Option<std::ops::Range<usize>>, text: &str, window: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| {
            view.clear_marked_text(cx);
            view.commit_text(text, cx);
        });
        window.invalidate_character_coordinates();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        new_text: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| view.set_marked_text(new_text.to_string(), cx));
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| view.clear_marked_text(cx));
    }

    fn bounds_for_range(&mut self, range_utf16: std::ops::Range<usize>, _: &mut Window, _: &mut App) -> Option<Bounds<Pixels>> {
        let mut bounds = self.cursor_bounds?;
        bounds.origin.x += self.cell_width * range_utf16.start as f32;
        Some(bounds)
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut App) -> Option<usize> {
        None
    }
}

#[allow(dead_code)]
fn mode_has_mouse(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_palette_matches_xterm() {
        assert_eq!(indexed_rgb(16), (0, 0, 0));
        assert_eq!(indexed_rgb(21), (0, 0, 255));
        assert_eq!(indexed_rgb(196), (255, 0, 0));
        assert_eq!(indexed_rgb(232), (8, 8, 8));
        assert_eq!(indexed_rgb(255), (238, 238, 238));
    }

    #[test]
    fn block_rects_merge() {
        let red = Hsla::red();
        let mut regions = Vec::new();
        assert!(collect_block_element_regions(LayoutPoint::new(0, 0), '█', red, &mut regions));
        assert!(collect_block_element_regions(LayoutPoint::new(0, 1), '█', red, &mut regions));
        let rects = block_element_regions_to_rects(regions);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].num_of_columns, 16);
        assert!(!collect_block_element_regions(LayoutPoint::new(0, 2), 'a', red, &mut Vec::new()));
    }

    #[test]
    fn selection_membership() {
        let sel = falcon_term::SnapSelection {
            start: (0, 5),
            end: (1, 2),
            is_block: false,
        };
        assert!(in_selection(&sel, 0, 5, 10));
        assert!(in_selection(&sel, 0, 9, 10));
        assert!(!in_selection(&sel, 0, 4, 10));
        assert!(in_selection(&sel, 1, 0, 10));
        assert!(!in_selection(&sel, 1, 3, 10));
    }
}
