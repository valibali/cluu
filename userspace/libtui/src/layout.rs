//! Layout primitives for libtui — the centerpiece of the TUI layout system.
//!
//! Provides:
//! - `Rect`: rectangular region in the terminal grid
//! - `Padding` / `Margin`: insets and offsets
//! - `Constraint`: declarative size constraints for splits
//! - `Border`: character-level border definition (lipgloss-style)
//! - `Block`: bordered box with optional title and padding
//! - `Drawable` / `Layoutable` traits: component layout contract
//! - `place` / `join_*_rects`: free helpers
//!
//! Pure arithmetic + View writes. No I/O. no_std + alloc.
//!
//! ## SOLID mapping
//!
//! - **SRP**: layout math is isolated from rendering, input, and runtime.
//! - **OCP**: new layout strategies = new `Constraint` variants or new
//!   functions; existing types unchanged.
//! - **ISP**: `Drawable` and `Layoutable` are separate narrow traits — a
//!   component implements only what it needs.
//! - **DIP**: components depend on `Rect`/`View` abstractions, never on
//!   the runtime.

extern crate alloc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::{Cell, View, COLOR_DEFAULT};

// =========================================================================
// Rect
// =========================================================================

/// A rectangle in the terminal grid (0-indexed, origin top-left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    pub const fn new(x: usize, y: usize, width: usize, height: usize) -> Self {
        Rect { x, y, width, height }
    }

    /// A zero-area rect at the origin.
    pub const fn zero() -> Self {
        Rect { x: 0, y: 0, width: 0, height: 0 }
    }

    /// Total cell count (width * height).
    pub const fn area(self) -> usize {
        self.width.saturating_mul(self.height)
    }

    /// Right edge (exclusive column).
    pub const fn right(self) -> usize {
        self.x.saturating_add(self.width)
    }

    /// Bottom edge (exclusive row).
    pub const fn bottom(self) -> usize {
        self.y.saturating_add(self.height)
    }

    /// Does this rect fully contain `other`?
    pub fn contains(self, other: Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// Does this rect contain the point (row, col)?
    pub fn contains_point(self, row: usize, col: usize) -> bool {
        row >= self.y && row < self.bottom() && col >= self.x && col < self.right()
    }

    /// Shrink by padding insets. Returns the interior rect.
    pub fn inner(self, padding: Padding) -> Rect {
        let x = self.x.saturating_add(padding.left);
        let y = self.y.saturating_add(padding.top);
        let width = self
            .width
            .saturating_sub(padding.left.saturating_add(padding.right));
        let height = self
            .height
            .saturating_sub(padding.top.saturating_add(padding.bottom));
        Rect { x, y, width, height }
    }

    /// Grow by margin offsets. Returns the exterior rect.
    pub fn outer(self, margin: Margin) -> Rect {
        let x = self.x.saturating_sub(margin.left);
        let y = self.y.saturating_sub(margin.top);
        let width = self.width.saturating_add(margin.left.saturating_add(margin.right));
        let height = self.height.saturating_add(margin.top.saturating_add(margin.bottom));
        Rect { x, y, width, height }
    }

    /// Split horizontally (left | right) using a constraint for the left
    /// portion. The right portion gets the remainder.
    pub fn split_h(self, constraint: Constraint) -> (Rect, Rect) {
        let left_w = constraint.solve(self.width);
        let right_w = self.width.saturating_sub(left_w);
        let left = Rect::new(self.x, self.y, left_w, self.height);
        let right = Rect::new(self.x + left_w, self.y, right_w, self.height);
        (left, right)
    }

    /// Split vertically (top | bottom) using a constraint for the top
    /// portion. The bottom portion gets the remainder.
    pub fn split_v(self, constraint: Constraint) -> (Rect, Rect) {
        let top_h = constraint.solve(self.height);
        let bot_h = self.height.saturating_sub(top_h);
        let top = Rect::new(self.x, self.y, self.width, top_h);
        let bot = Rect::new(self.x, self.y + top_h, self.width, bot_h);
        (top, bot)
    }

    /// Split into rows top-to-bottom using constraints. Remaining space
    /// after all constraints is discarded.
    pub fn rows(self, constraints: &[Constraint]) -> Vec<Rect> {
        solve_constraints(constraints, self.height)
            .iter()
            .scan(self.y, |y, &h| {
                let r = Rect::new(self.x, *y, self.width, h);
                *y = y.saturating_add(h);
                Some(r)
            })
            .collect()
    }

    /// Split into columns left-to-right using constraints.
    pub fn cols(self, constraints: &[Constraint]) -> Vec<Rect> {
        solve_constraints(constraints, self.width)
            .iter()
            .scan(self.x, |x, &w| {
                let r = Rect::new(*x, self.y, w, self.height);
                *x = x.saturating_add(w);
                Some(r)
            })
            .collect()
    }

    /// Place a rect of size (w, h) within self at the given alignment.
    /// Returns the positioned rect. Clamps w/h to fit.
    pub fn place(self, w: usize, h: usize, ha: Position, va: VPosition) -> Rect {
        let w = w.min(self.width);
        let h = h.min(self.height);
        let x = self.x + match ha {
            Position::Left => 0,
            Position::Center => (self.width - w) / 2,
            Position::Right => self.width - w,
        };
        let y = self.y + match va {
            VPosition::Top => 0,
            VPosition::Center => (self.height - h) / 2,
            VPosition::Bottom => self.height - h,
        };
        Rect::new(x, y, w, h)
    }
}

// =========================================================================
// Padding / Margin
// =========================================================================

/// Interior insets — shrinks a Rect inward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Padding {
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
    pub left: usize,
}

impl Padding {
    pub const fn all(n: usize) -> Self {
        Padding { top: n, right: n, bottom: n, left: n }
    }

    pub const fn horizontal(n: usize) -> Self {
        Padding { top: 0, right: n, bottom: 0, left: n }
    }

    pub const fn vertical(n: usize) -> Self {
        Padding { top: n, right: 0, bottom: n, left: 0 }
    }

    pub const fn trbl(top: usize, right: usize, bottom: usize, left: usize) -> Self {
        Padding { top, right, bottom, left }
    }

    pub const fn zero() -> Self {
        Padding { top: 0, right: 0, bottom: 0, left: 0 }
    }
}

/// Exterior offsets — grows a Rect outward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Margin {
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
    pub left: usize,
}

impl Margin {
    pub const fn all(n: usize) -> Self {
        Margin { top: n, right: n, bottom: n, left: n }
    }

    pub const fn horizontal(n: usize) -> Self {
        Margin { top: 0, right: n, bottom: 0, left: n }
    }

    pub const fn vertical(n: usize) -> Self {
        Margin { top: n, right: 0, bottom: n, left: 0 }
    }

    pub const fn zero() -> Self {
        Margin { top: 0, right: 0, bottom: 0, left: 0 }
    }
}

// =========================================================================
// Constraint — declarative sizing for splits
// =========================================================================

/// Size constraint for layout splits. Resolved against available space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constraint {
    /// Fixed number of cells.
    Length(usize),
    /// Percentage of available space (0..=100).
    Percentage(u16),
    /// Proportional share of remaining space after fixed constraints.
    /// Weight is relative to other Fill constraints.
    Fill(u16),
    /// At least N cells; grows to absorb remaining space if no Fill.
    Min(usize),
    /// At most N cells; shrinks if space is tight.
    Max(usize),
}

impl Constraint {
    /// Solve a single constraint against a total size.
    pub fn solve(self, total: usize) -> usize {
        match self {
            Constraint::Length(n) => n.min(total),
            Constraint::Percentage(p) => (total.saturating_mul(p as usize)) / 100,
            Constraint::Fill(_) => total, // standalone Fill takes everything
            Constraint::Min(n) => n.min(total),
            Constraint::Max(n) => n.min(total),
        }
    }
}

/// Solve multiple constraints against a total size, distributing space.
///
/// Resolution order:
/// 1. `Length` — exact, deducted from remaining
/// 2. `Percentage` — of total, deducted from remaining
/// 3. `Min` — minimum, deducted from remaining
/// 4. `Fill` — proportional share of what remains (by weight)
/// 5. `Max` — clamp result to maximum
fn solve_constraints(constraints: &[Constraint], total: usize) -> Vec<usize> {
    let n = constraints.len();
    let mut sizes = vec![0usize; n];
    let mut remaining = total;

    // Pass 1: Length
    for (i, c) in constraints.iter().enumerate() {
        if let Constraint::Length(len) = c {
            let s = (*len).min(remaining);
            sizes[i] = s;
            remaining = remaining.saturating_sub(s);
        }
    }

    // Pass 2: Percentage (of total, not remaining)
    for (i, c) in constraints.iter().enumerate() {
        if let Constraint::Percentage(pct) = c {
            let s = ((total.saturating_mul(*pct as usize)) / 100).min(remaining);
            sizes[i] = s;
            remaining = remaining.saturating_sub(s);
        }
    }

    // Pass 3: Min
    for (i, c) in constraints.iter().enumerate() {
        if let Constraint::Min(min) = c {
            let s = (*min).min(remaining);
            sizes[i] = s;
            remaining = remaining.saturating_sub(s);
        }
    }

    // Pass 4: Fill — proportional distribution of remaining
    let fill_weight_sum: u16 = constraints.iter().filter_map(|c| {
        if let Constraint::Fill(w) = c { Some(*w) } else { None }
    }).sum();

    if fill_weight_sum > 0 && remaining > 0 {
        let mut allocated = 0usize;
        let mut last_fill = 0;
        for (i, c) in constraints.iter().enumerate() {
            if let Constraint::Fill(w) = c {
                last_fill = i;
                let s = (remaining.saturating_mul(*w as usize)) / fill_weight_sum as usize;
                sizes[i] = s;
                allocated = allocated.saturating_add(s);
            }
        }
        // Distribute rounding remainder to the last Fill constraint
        if allocated < remaining {
            sizes[last_fill] = sizes[last_fill].saturating_add(remaining - allocated);
        }
    }

    // Pass 5: Max — clamp (and give back freed space, unused for now)
    for (i, c) in constraints.iter().enumerate() {
        if let Constraint::Max(max) = c {
            if sizes[i] > *max {
                sizes[i] = *max;
            }
        }
    }

    sizes
}

// =========================================================================
// Position / VPosition — alignment
// =========================================================================

/// Horizontal alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    #[default]
    Left,
    Center,
    Right,
}

/// Vertical alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VPosition {
    #[default]
    Top,
    Center,
    Bottom,
}

// =========================================================================
// Border — character-level border (lipgloss-style)
// =========================================================================

/// Character-level border definition. Each field is the rune for that edge.
/// All spaces = no visible border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Border {
    pub top: char,
    pub bottom: char,
    pub left: char,
    pub right: char,
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
}

impl Border {
    /// Empty border (all spaces).
    pub const fn new() -> Self {
        Border {
            top: ' ', bottom: ' ', left: ' ', right: ' ',
            top_left: ' ', top_right: ' ', bottom_left: ' ', bottom_right: ' ',
        }
    }

    /// Single-line: `─ │ ┌ ┐ └ ┘`
    pub const fn single() -> Self {
        Border {
            top: '─', bottom: '─', left: '│', right: '│',
            top_left: '┌', top_right: '┐', bottom_left: '└', bottom_right: '┘',
        }
    }

    /// Rounded: `─ │ ╭ ╮ ╰ ╯`
    pub const fn rounded() -> Self {
        Border {
            top: '─', bottom: '─', left: '│', right: '│',
            top_left: '╭', top_right: '╮', bottom_left: '╰', bottom_right: '╯',
        }
    }

    /// Double: `═ ║ ╔ ╗ ╚ ╝`
    pub const fn double() -> Self {
        Border {
            top: '═', bottom: '═', left: '║', right: '║',
            top_left: '╔', top_right: '╗', bottom_left: '╚', bottom_right: '╝',
        }
    }

    /// Thick: `━ ┃ ┏ ┓ ┗ ┛`
    pub const fn thick() -> Self {
        Border {
            top: '━', bottom: '━', left: '┃', right: '┃',
            top_left: '┏', top_right: '┓', bottom_left: '┗', bottom_right: '┛',
        }
    }

    /// Is this border visually empty?
    pub const fn is_empty(self) -> bool {
        // All space chars = no border. Check the edge chars.
        self.top == ' ' && self.bottom == ' ' && self.left == ' ' && self.right == ' '
    }
}

impl Default for Border {
    fn default() -> Self {
        Border::new()
    }
}

/// Bridge from the legacy style::Border enum.
impl From<crate::style::Border> for Border {
    fn from(old: crate::style::Border) -> Self {
        match old {
            crate::style::Border::None => Border::new(),
            crate::style::Border::Single => Border::single(),
            crate::style::Border::Double => Border::double(),
            crate::style::Border::Rounded => Border::rounded(),
        }
    }
}

// =========================================================================
// Block — bordered box with title and padding
// =========================================================================

/// A bordered box with optional title and padding. The primary layout
/// container — draws a frame, title, and background fill, then exposes
/// the interior rect for content.
pub struct Block {
    border: Border,
    padding: Padding,
    title: Option<String>,
    border_fg: u8,
    bg: u8,
}

impl Block {
    pub fn new() -> Self {
        Block {
            border: Border::new(),
            padding: Padding::zero(),
            title: None,
            border_fg: COLOR_DEFAULT,
            bg: COLOR_DEFAULT,
        }
    }

    /// Set the border style.
    pub fn border(mut self, border: Border) -> Self {
        self.border = border;
        self
    }

    /// Set the title shown in the top border.
    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(String::from(title));
        self
    }

    /// Set interior padding.
    pub fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }

    /// Set border foreground color.
    pub fn border_fg(mut self, fg: u8) -> Self {
        self.border_fg = fg;
        self
    }

    /// Set background fill color for the interior.
    pub fn bg(mut self, bg: u8) -> Self {
        self.bg = bg;
        self
    }

    /// Compute the interior rect: area minus border minus padding.
    pub fn inner(&self, area: Rect) -> Rect {
        let border_h = if self.border.is_empty() { 0 } else { 1 };
        let border_v = if self.border.is_empty() { 0 } else { 1 };
        let total_padding = Padding {
            top: self.padding.top + border_h,
            right: self.padding.right + border_v,
            bottom: self.padding.bottom + border_h,
            left: self.padding.left + border_v,
        };
        area.inner(total_padding)
    }

    /// Draw the block (border + title + background fill) into `buf` at `area`.
    pub fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Fill background
        if self.bg != COLOR_DEFAULT {
            buf.fill_rect(area.y, area.x, area.width, area.height, Cell::new(' ').bg(self.bg));
        }

        if self.border.is_empty() {
            return;
        }

        let last_row = area.bottom() - 1;
        let last_col = area.right() - 1;

        // Corners (only if area is big enough)
        if area.width >= 2 && area.height >= 2 {
            buf.set(area.y, area.x, Cell::new(self.border.top_left).fg(self.border_fg));
            buf.set(area.y, last_col, Cell::new(self.border.top_right).fg(self.border_fg));
            buf.set(last_row, area.x, Cell::new(self.border.bottom_left).fg(self.border_fg));
            buf.set(last_row, last_col, Cell::new(self.border.bottom_right).fg(self.border_fg));
        }

        // Top and bottom edges
        if area.width > 2 && area.height >= 1 {
            for c in (area.x + 1)..last_col {
                if area.height >= 2 {
                    buf.set(area.y, c, Cell::new(self.border.top).fg(self.border_fg));
                    buf.set(last_row, c, Cell::new(self.border.bottom).fg(self.border_fg));
                }
            }
        }

        // Left and right edges
        if area.height > 2 && area.width >= 1 {
            for r in (area.y + 1)..last_row {
                if area.width >= 2 {
                    buf.set(r, area.x, Cell::new(self.border.left).fg(self.border_fg));
                    buf.set(r, last_col, Cell::new(self.border.right).fg(self.border_fg));
                }
            }
        }

        // Title in top border
        if let Some(ref title) = self.title {
            if area.width > 4 && area.height >= 2 {
                let title_start = area.x + 2; // skip corner + space
                for (i, ch) in title.chars().enumerate() {
                    let col = title_start + i;
                    if col >= last_col - 1 {
                        break;
                    }
                    buf.set(area.y, col, Cell::new(ch).fg(self.border_fg));
                }
            }
        }
    }
}

impl Default for Block {
    fn default() -> Self {
        Block::new()
    }
}

impl Drawable for Block {
    fn draw(&self, area: Rect, buf: &mut View) {
        // Delegate to the inherent draw method.
        self.draw(area, buf);
    }
}

// =========================================================================
// Drawable / Layoutable traits — the component contract
// =========================================================================

/// Something that can be drawn into a View at a given Rect.
///
/// This is the primary trait for all TUI components. A component receives
/// its allocated area and writes cells directly into the buffer.
pub trait Drawable {
    fn draw(&self, area: Rect, buf: &mut View);
}

/// Something that can report its preferred size given a maximum.
///
/// Used by layout containers to negotiate space allocation.
pub trait Layoutable {
    /// Returns (width, height) — the preferred size, clamped to max.
    fn measure(&self, max_w: usize, max_h: usize) -> (usize, usize);
}

// =========================================================================
// Free helpers
// =========================================================================

/// Place a rect of size (w, h) within `area` at the given alignment.
pub fn place(area: Rect, w: usize, h: usize, ha: Position, va: VPosition) -> Rect {
    area.place(w, h, ha, va)
}

/// Bounding union of two rects (smallest rect containing both).
pub fn join_horizontal_rects(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
}

/// Same as `join_horizontal_rects` — bounding union is direction-agnostic.
pub fn join_vertical_rects(a: Rect, b: Rect) -> Rect {
    join_horizontal_rects(a, b)
}

// =========================================================================
// Flex — flexbox-inspired container (main-axis / cross-axis layout)
// =========================================================================

/// Layout direction for `Flex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    Row,
    Column,
}

/// Main-axis distribution of leftover space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
}

/// A single item in a `Flex` layout, with a size constraint and optional
/// per-item cross-axis alignment override.
pub struct FlexItem {
    constraint: Constraint,
    align_override: Option<Position>,
}

impl FlexItem {
    pub fn new(constraint: Constraint) -> Self {
        FlexItem { constraint, align_override: None }
    }

    pub fn align(mut self, alignment: Position) -> Self {
        self.align_override = Some(alignment);
        self
    }
}

fn cross_axis_offset(item_size: usize, container_size: usize, align: Position) -> usize {
    match align {
        Position::Left => 0,
        Position::Center => (container_size.saturating_sub(item_size)) / 2,
        Position::Right => container_size.saturating_sub(item_size),
    }
}

/// Flexbox-inspired layout container. Distributes items along a main axis
/// (Row = horizontal, Column = vertical) with justify-content semantics,
/// and aligns each item on the cross axis.
///
/// Excludes flex-wrap, flex-order, and flex-shrink — terminal UIs want
/// explicit layout, not reflow. What it adds over `Rect::rows`/`cols`:
/// - `justify`: leftover space distribution (Start/Center/End/SpaceBetween)
/// - `align`: cross-axis alignment per item (or per-item override)
/// - `gap`: uniform spacing between items
pub struct Flex {
    direction: Direction,
    justify: Justify,
    align: Position,
    gap: usize,
}

impl Flex {
    pub fn row() -> Self {
        Flex { direction: Direction::Row, justify: Justify::default(), align: Position::default(), gap: 0 }
    }

    pub fn column() -> Self {
        Flex { direction: Direction::Column, justify: Justify::default(), align: Position::default(), gap: 0 }
    }

    pub fn justify(mut self, j: Justify) -> Self {
        self.justify = j;
        self
    }

    pub fn align(mut self, a: Position) -> Self {
        self.align = a;
        self
    }

    pub fn gap(mut self, n: usize) -> Self {
        self.gap = n;
        self
    }

    /// Lay out items within `area`. Returns one positioned `Rect` per item.
    /// Items get their main-axis size from their constraint (resolved against
    /// available space minus gaps); cross-axis size fills the container
    /// unless an align override shrinks the item on the cross axis.
    pub fn layout(&self, area: Rect, items: &[FlexItem]) -> Vec<Rect> {
        if items.is_empty() || area.width == 0 || area.height == 0 {
            return Vec::new();
        }

        let constraints: Vec<Constraint> = items.iter().map(|i| i.constraint).collect();
        let total_gap = self.gap.saturating_mul(items.len().saturating_sub(1));

        let (main_size, cross_size, main_start, cross_start) = match self.direction {
            Direction::Row => (area.width.saturating_sub(total_gap), area.height, area.x, area.y),
            Direction::Column => (area.height.saturating_sub(total_gap), area.width, area.y, area.x),
        };

        let sizes = solve_constraints(&constraints, main_size);
        let used: usize = sizes.iter().sum();

        let leading_gap = match self.justify {
            Justify::Start => 0,
            Justify::Center => (main_size - used) / 2,
            Justify::End => main_size - used,
            Justify::SpaceBetween => 0,
        };

        let between_gap = if self.justify == Justify::SpaceBetween && items.len() > 1 {
            (main_size - used) / (items.len() - 1)
        } else {
            self.gap
        };

        let mut results = Vec::with_capacity(items.len());
        let mut pos = main_start + leading_gap;

        for (i, &size) in sizes.iter().enumerate() {
            if i > 0 {
                pos = pos.saturating_add(between_gap);
            }

            let item_align = items[i].align_override.unwrap_or(self.align);

            let rect = match self.direction {
                Direction::Row => {
                    let y = cross_start + cross_axis_offset(0, cross_size, item_align);
                    Rect::new(pos, y, size, cross_size)
                }
                Direction::Column => {
                    let x = cross_start + cross_axis_offset(0, cross_size, item_align);
                    Rect::new(x, pos, cross_size, size)
                }
            };
            results.push(rect);

            pos = pos.saturating_add(size);
        }

        results
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- Rect arithmetic ---

    #[test]
    fn rect_new() {
        let r = Rect::new(1, 2, 3, 4);
        assert_eq!(r.x, 1);
        assert_eq!(r.y, 2);
        assert_eq!(r.width, 3);
        assert_eq!(r.height, 4);
    }

    #[test]
    fn rect_area() {
        assert_eq!(Rect::new(0, 0, 3, 4).area(), 12);
        assert_eq!(Rect::zero().area(), 0);
    }

    #[test]
    fn rect_right_bottom() {
        let r = Rect::new(5, 10, 3, 4);
        assert_eq!(r.right(), 8);
        assert_eq!(r.bottom(), 14);
    }

    #[test]
    fn rect_contains() {
        let outer = Rect::new(0, 0, 10, 10);
        let inner = Rect::new(2, 2, 5, 5);
        assert!(outer.contains(inner));
        assert!(!inner.contains(outer));
        assert!(outer.contains(Rect::new(0, 0, 10, 10)));
        assert!(!outer.contains(Rect::new(9, 9, 2, 2)));
    }

    #[test]
    fn rect_contains_point() {
        let r = Rect::new(2, 3, 4, 5);
        assert!(r.contains_point(3, 4));
        assert!(r.contains_point(7, 5)); // right-1, bottom-1
        assert!(!r.contains_point(2, 1)); // above
        assert!(!r.contains_point(6, 8)); // below
    }

    #[test]
    fn rect_inner_with_padding() {
        let r = Rect::new(10, 10, 20, 10);
        let inner = r.inner(Padding::all(2));
        assert_eq!(inner, Rect::new(12, 12, 16, 6));
    }

    #[test]
    fn rect_inner_zero_padding() {
        let r = Rect::new(0, 0, 5, 5);
        assert_eq!(r.inner(Padding::zero()), r);
    }

    #[test]
    fn rect_inner_oversized_padding() {
        let r = Rect::new(0, 0, 3, 3);
        let inner = r.inner(Padding::all(5));
        assert_eq!(inner.width, 0);
        assert_eq!(inner.height, 0);
    }

    #[test]
    fn rect_outer_with_margin() {
        let r = Rect::new(5, 5, 10, 10);
        let ext = r.outer(Margin::all(2));
        assert_eq!(ext, Rect::new(3, 3, 14, 14));
    }

    // --- Splits ---

    #[test]
    fn rect_split_h_length() {
        let r = Rect::new(0, 0, 10, 5);
        let (left, right) = r.split_h(Constraint::Length(3));
        assert_eq!(left, Rect::new(0, 0, 3, 5));
        assert_eq!(right, Rect::new(3, 0, 7, 5));
    }

    #[test]
    fn rect_split_v_percentage() {
        let r = Rect::new(0, 0, 10, 10);
        let (top, bottom) = r.split_v(Constraint::Percentage(30));
        assert_eq!(top, Rect::new(0, 0, 10, 3));
        assert_eq!(bottom, Rect::new(0, 3, 10, 7));
    }

    #[test]
    fn rect_rows_mixed_constraints() {
        let r = Rect::new(0, 0, 20, 10);
        let rows = r.rows(&[
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(2),
        ]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], Rect::new(0, 0, 20, 3));
        assert_eq!(rows[1], Rect::new(0, 3, 20, 5)); // 10 - 3 - 2 = 5
        assert_eq!(rows[2], Rect::new(0, 8, 20, 2));
    }

    #[test]
    fn rect_cols_fill_weights() {
        let r = Rect::new(0, 0, 10, 5);
        let cols = r.cols(&[Constraint::Fill(1), Constraint::Fill(3)]);
        assert_eq!(cols.len(), 2);
        // 1:3 ratio of 10 = 2.5 → 2 and 8 (rounding goes to last)
        assert_eq!(cols[0].width + cols[1].width, 10);
    }

    #[test]
    fn rect_cols_all_length() {
        let r = Rect::new(0, 0, 10, 5);
        let cols = r.cols(&[Constraint::Length(3), Constraint::Length(4), Constraint::Length(3)]);
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0], Rect::new(0, 0, 3, 5));
        assert_eq!(cols[1], Rect::new(3, 0, 4, 5));
        assert_eq!(cols[2], Rect::new(7, 0, 3, 5));
    }

    #[test]
    fn rect_rows_overflow_clamped() {
        let r = Rect::new(0, 0, 10, 5);
        let rows = r.rows(&[Constraint::Length(3), Constraint::Length(4)]);
        // 3 + 4 = 7 > 5, so Length(4) clamps to remaining 2
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], Rect::new(0, 0, 10, 3));
        assert_eq!(rows[1], Rect::new(0, 3, 10, 2));
    }

    #[test]
    fn rect_place_center() {
        let area = Rect::new(0, 0, 10, 10);
        let inner = area.place(4, 2, Position::Center, VPosition::Center);
        assert_eq!(inner, Rect::new(3, 4, 4, 2));
    }

    #[test]
    fn rect_place_top_left() {
        let area = Rect::new(5, 5, 10, 10);
        let inner = area.place(3, 3, Position::Left, VPosition::Top);
        assert_eq!(inner, Rect::new(5, 5, 3, 3));
    }

    #[test]
    fn rect_place_bottom_right() {
        let area = Rect::new(0, 0, 10, 10);
        let inner = area.place(3, 3, Position::Right, VPosition::Bottom);
        assert_eq!(inner, Rect::new(7, 7, 3, 3));
    }

    #[test]
    fn rect_place_clamps() {
        let area = Rect::new(0, 0, 5, 5);
        let inner = area.place(10, 10, Position::Center, VPosition::Center);
        assert_eq!(inner, Rect::new(0, 0, 5, 5));
    }

    // --- Constraint solver ---

    #[test]
    fn constraint_solve_length() {
        assert_eq!(Constraint::Length(5).solve(10), 5);
        assert_eq!(Constraint::Length(15).solve(10), 10); // clamped
    }

    #[test]
    fn constraint_solve_percentage() {
        assert_eq!(Constraint::Percentage(50).solve(10), 5);
        assert_eq!(Constraint::Percentage(33).solve(100), 33);
    }

    #[test]
    fn constraint_solve_fill() {
        assert_eq!(Constraint::Fill(1).solve(10), 10);
    }

    #[test]
    fn constraint_solve_min() {
        assert_eq!(Constraint::Min(3).solve(10), 3);
        assert_eq!(Constraint::Min(15).solve(10), 10);
    }

    #[test]
    fn constraint_solve_max() {
        assert_eq!(Constraint::Max(3).solve(10), 3);
    }

    #[test]
    fn solve_constraints_fill_remaining() {
        let sizes = solve_constraints(&[
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(2),
        ], 10);
        assert_eq!(sizes, vec![3, 5, 2]);
    }

    #[test]
    fn solve_constraints_fill_proportional() {
        let sizes = solve_constraints(&[
            Constraint::Fill(1),
            Constraint::Fill(3),
        ], 10);
        assert_eq!(sizes[0] + sizes[1], 10);
        // 1:3 ratio → 2 and 8 (or close, with rounding to last)
        assert!(sizes[1] >= sizes[0]); // Fill(3) gets at least as much
    }

    // --- Border ---

    #[test]
    fn border_single_chars() {
        let b = Border::single();
        assert_eq!(b.top, '─');
        assert_eq!(b.left, '│');
        assert_eq!(b.top_left, '┌');
        assert_eq!(b.bottom_right, '┘');
    }

    #[test]
    fn border_rounded_chars() {
        let b = Border::rounded();
        assert_eq!(b.top_left, '╭');
        assert_eq!(b.top_right, '╮');
        assert_eq!(b.bottom_left, '╰');
        assert_eq!(b.bottom_right, '╯');
    }

    #[test]
    fn border_double_chars() {
        let b = Border::double();
        assert_eq!(b.top, '═');
        assert_eq!(b.left, '║');
        assert_eq!(b.top_left, '╔');
    }

    #[test]
    fn border_thick_chars() {
        let b = Border::thick();
        assert_eq!(b.top, '━');
        assert_eq!(b.left, '┃');
        assert_eq!(b.top_left, '┏');
    }

    #[test]
    fn border_empty_is_empty() {
        assert!(Border::new().is_empty());
        assert!(!Border::single().is_empty());
    }

    #[test]
    fn border_from_style_border() {
        assert_eq!(Border::from(crate::style::Border::Single), Border::single());
        assert_eq!(Border::from(crate::style::Border::Double), Border::double());
        assert_eq!(Border::from(crate::style::Border::Rounded), Border::rounded());
        assert_eq!(Border::from(crate::style::Border::None), Border::new());
    }

    // --- Block ---

    #[test]
    fn block_inner_no_border_no_padding() {
        let block = Block::new();
        let area = Rect::new(0, 0, 10, 10);
        assert_eq!(block.inner(area), area);
    }

    #[test]
    fn block_inner_with_border() {
        let block = Block::new().border(Border::single());
        let area = Rect::new(0, 0, 10, 10);
        let inner = block.inner(area);
        assert_eq!(inner, Rect::new(1, 1, 8, 8));
    }

    #[test]
    fn block_inner_with_border_and_padding() {
        let block = Block::new()
            .border(Border::single())
            .padding(Padding::all(2));
        let area = Rect::new(0, 0, 10, 10);
        let inner = block.inner(area);
        assert_eq!(inner, Rect::new(3, 3, 4, 4));
    }

    #[test]
    fn block_draw_border_corners() {
        let block = Block::new().border(Border::single());
        let mut buf = View::new(10, 5);
        let area = Rect::new(0, 0, 10, 5);
        block.draw(area, &mut buf);

        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('┌'));
        assert_eq!(buf.get(0, 9).map(|c| c.ch), Some('┐'));
        assert_eq!(buf.get(4, 0).map(|c| c.ch), Some('└'));
        assert_eq!(buf.get(4, 9).map(|c| c.ch), Some('┘'));
    }

    #[test]
    fn block_draw_border_edges() {
        let block = Block::new().border(Border::single());
        let mut buf = View::new(10, 5);
        block.draw(Rect::new(0, 0, 10, 5), &mut buf);

        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some('─'));
        assert_eq!(buf.get(4, 3).map(|c| c.ch), Some('─'));
        assert_eq!(buf.get(2, 0).map(|c| c.ch), Some('│'));
        assert_eq!(buf.get(2, 9).map(|c| c.ch), Some('│'));
    }

    #[test]
    fn block_draw_title() {
        let block = Block::new().border(Border::single()).title("Test");
        let mut buf = View::new(20, 5);
        block.draw(Rect::new(0, 0, 20, 5), &mut buf);

        // Title starts at col 2 (after corner + space)
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some('T'));
        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some('e'));
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('s'));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some('t'));
    }

    #[test]
    fn block_draw_background_fill() {
        let block = Block::new().border(Border::single()).bg(4);
        let mut buf = View::new(10, 5);
        block.draw(Rect::new(0, 0, 10, 5), &mut buf);

        // Interior should have bg=4
        assert_eq!(buf.get(2, 3).map(|c| c.bg), Some(4));
        // Border should have default bg
        assert_eq!(buf.get(0, 0).map(|c| c.bg), Some(COLOR_DEFAULT));
    }

    #[test]
    fn block_draw_no_border_just_bg() {
        let block = Block::new().bg(3);
        let mut buf = View::new(5, 3);
        block.draw(Rect::new(0, 0, 5, 3), &mut buf);

        // All cells should have bg=3
        for r in 0..3 {
            for c in 0..5 {
                assert_eq!(buf.get(r, c).map(|c| c.bg), Some(3));
            }
        }
    }

    #[test]
    fn block_draw_too_small_skips_border() {
        let block = Block::new().border(Border::single());
        let mut buf = View::new(5, 3);
        // 1x1 area — too small for corners, should not crash
        block.draw(Rect::new(0, 0, 1, 1), &mut buf);
        // No crash = pass
    }

    #[test]
    fn block_draw_zero_area_noop() {
        let block = Block::new().border(Border::single());
        let mut buf = View::new(5, 5);
        block.draw(Rect::zero(), &mut buf);
        // All spaces = no change
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn block_draw_with_offset() {
        let block = Block::new().border(Border::single());
        let mut buf = View::new(15, 10);
        block.draw(Rect::new(2, 3, 10, 5), &mut buf);

        assert_eq!(buf.get(3, 2).map(|c| c.ch), Some('┌'));
        assert_eq!(buf.get(3, 11).map(|c| c.ch), Some('┐'));
        assert_eq!(buf.get(7, 2).map(|c| c.ch), Some('└'));
        assert_eq!(buf.get(7, 11).map(|c| c.ch), Some('┘'));
    }

    // --- join_*_rects ---

    #[test]
    fn join_rects_bounding_union() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(3, 3, 5, 5);
        let u = join_horizontal_rects(a, b);
        assert_eq!(u, Rect::new(0, 0, 8, 8));
    }

    #[test]
    fn join_rects_disjoint() {
        let a = Rect::new(0, 0, 3, 3);
        let b = Rect::new(10, 10, 3, 3);
        let u = join_vertical_rects(a, b);
        assert_eq!(u, Rect::new(0, 0, 13, 13));
    }

    // --- Flex ---

    #[test]
    fn flex_row_basic_lengths() {
        let area = Rect::new(0, 0, 10, 3);
        let items = vec![
            FlexItem::new(Constraint::Length(3)),
            FlexItem::new(Constraint::Length(4)),
            FlexItem::new(Constraint::Length(3)),
        ];
        let rects = Flex::row().layout(area, &items);
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0], Rect::new(0, 0, 3, 3));
        assert_eq!(rects[1], Rect::new(3, 0, 4, 3));
        assert_eq!(rects[2], Rect::new(7, 0, 3, 3));
    }

    #[test]
    fn flex_column_basic_lengths() {
        let area = Rect::new(0, 0, 5, 10);
        let items = vec![
            FlexItem::new(Constraint::Length(3)),
            FlexItem::new(Constraint::Length(4)),
        ];
        let rects = Flex::column().layout(area, &items);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0], Rect::new(0, 0, 5, 3));
        assert_eq!(rects[1], Rect::new(0, 3, 5, 4));
    }

    #[test]
    fn flex_row_fill_distributes() {
        let area = Rect::new(0, 0, 10, 3);
        let items = vec![
            FlexItem::new(Constraint::Length(2)),
            FlexItem::new(Constraint::Fill(1)),
            FlexItem::new(Constraint::Length(2)),
        ];
        let rects = Flex::row().layout(area, &items);
        assert_eq!(rects[0].width, 2);
        assert_eq!(rects[1].width, 6);
        assert_eq!(rects[2].width, 2);
        assert_eq!(rects[1].x, 2);
        assert_eq!(rects[2].x, 8);
    }

    #[test]
    fn flex_justify_center() {
        let area = Rect::new(0, 0, 10, 3);
        let items = vec![
            FlexItem::new(Constraint::Length(2)),
            FlexItem::new(Constraint::Length(2)),
        ];
        let rects = Flex::row().justify(Justify::Center).layout(area, &items);
        assert_eq!(rects[0].x, 3);
        assert_eq!(rects[1].x, 5);
    }

    #[test]
    fn flex_justify_end() {
        let area = Rect::new(0, 0, 10, 3);
        let items = vec![FlexItem::new(Constraint::Length(3))];
        let rects = Flex::row().justify(Justify::End).layout(area, &items);
        assert_eq!(rects[0].x, 7);
    }

    #[test]
    fn flex_justify_space_between() {
        let area = Rect::new(0, 0, 10, 3);
        let items = vec![
            FlexItem::new(Constraint::Length(2)),
            FlexItem::new(Constraint::Length(2)),
            FlexItem::new(Constraint::Length(2)),
        ];
        let rects = Flex::row().justify(Justify::SpaceBetween).layout(area, &items);
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[2].x, 8);
        assert!(rects[1].x > 0 && rects[1].x < 8);
    }

    #[test]
    fn flex_gap_between_items() {
        let area = Rect::new(0, 0, 10, 3);
        let items = vec![
            FlexItem::new(Constraint::Length(2)),
            FlexItem::new(Constraint::Length(2)),
            FlexItem::new(Constraint::Length(2)),
        ];
        let rects = Flex::row().gap(1).layout(area, &items);
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[1].x, 3);
        assert_eq!(rects[2].x, 6);
    }

    #[test]
    fn flex_empty_items_returns_empty() {
        let area = Rect::new(0, 0, 10, 3);
        let rects = Flex::row().layout(area, &[]);
        assert!(rects.is_empty());
    }

    #[test]
    fn flex_zero_area_returns_empty() {
        let area = Rect::zero();
        let items = vec![FlexItem::new(Constraint::Length(2))];
        let rects = Flex::row().layout(area, &items);
        assert!(rects.is_empty());
    }

    #[test]
    fn flex_cross_axis_fills() {
        let area = Rect::new(0, 0, 10, 5);
        let items = vec![FlexItem::new(Constraint::Length(3))];
        let rects = Flex::row().layout(area, &items);
        assert_eq!(rects[0].height, 5);
    }

    #[test]
    fn flex_row_with_offset() {
        let area = Rect::new(2, 3, 10, 5);
        let items = vec![FlexItem::new(Constraint::Length(3))];
        let rects = Flex::row().layout(area, &items);
        assert_eq!(rects[0], Rect::new(2, 3, 3, 5));
    }

    #[test]
    fn flex_column_fill() {
        let area = Rect::new(0, 0, 5, 10);
        let items = vec![
            FlexItem::new(Constraint::Length(2)),
            FlexItem::new(Constraint::Fill(1)),
        ];
        let rects = Flex::column().layout(area, &items);
        assert_eq!(rects[0].height, 2);
        assert_eq!(rects[1].height, 8);
    }
}
