//! Reusable TUI components for libtui.
//!
//! Provides viewport (scrollable content pane), textinput (single-line
//! input), list (filterable, paginated list), browser (modal file/directory
//! picker), progress (progress bar), spinner (loading indicator), gauge
//! (value bar), sparkline (time-series chart), text (styled paragraph),
//! table (columns/rows), divider (separator line), badge (status label),
//! helpline (keybinding display), statusbar (bottom bar), tabs (tab bar),
//! tree (collapsible tree), barchart (bar chart), checkbox (toggle),
//! numberinput (stepper), and modal (overlay dialog) — building blocks
//! for Elm-style MVU applications.

pub mod viewport;
pub mod textinput;
pub mod list;
pub mod browser;
pub mod progress;
pub mod spinner;
pub mod gauge;
pub mod sparkline;
pub mod text;
pub mod table;
pub mod divider;
pub mod badge;
pub mod helpline;
pub mod statusbar;
pub mod tabs;
pub mod tree;
pub mod barchart;
pub mod checkbox;
pub mod numberinput;
pub mod modal;
pub mod bigtext;
pub mod filedialog;
pub mod scrollbar;
pub mod button;
pub mod marquee;
pub mod canvas;
pub mod pixel;
pub mod treebuilder;
