//! Cluuamp model: playback state, UI state, focus, tick logic, key handling.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libcluu::Result;
use libtui::components::browser::DirEntry;
use libtui::components::filedialog::{DialogAction, FileDialog};
use libtui::input::KeyEvent;

use crate::audio::{AudioEngine, PlaybackState};
use crate::fft::SpectrumAnalyzer;
use crate::layout::Layout;
use crate::scope::Oscilloscope;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VisMode {
    Spectrum,
    Oscilloscope,
}

impl VisMode {
    pub fn toggle(self) -> Self {
        match self {
            VisMode::Spectrum => VisMode::Oscilloscope,
            VisMode::Oscilloscope => VisMode::Spectrum,
        }
    }
}

pub use crate::layout::FocusArea;

pub struct CluuampModel {
    pub audio: AudioEngine,
    pub fft: SpectrumAnalyzer,
    pub scope: Oscilloscope,
    pub vis_mode: VisMode,
    pub focus: FocusArea,
    pub eq_enabled: bool,
    pub show_eq: bool,
    pub show_playlist: bool,
    pub eq_bands: [i8; 11],
    pub eq_selected: usize,
    pub shuffle: bool,
    pub repeat: bool,
    pub title_scroll_offset: usize,
    pub last_rendered_scroll: usize,
    pub scroll_accumulator: usize,
    pub playlist_scroll: usize,
    pub playlist_selected: usize,
    pub layout: Layout,
    pub transport_selected: usize,
    pub should_quit: bool,
    pub browser: Option<FileDialog>,
    pub pending_dir_list: Option<String>,
    pub pending_dir_import: Option<String>,
    pub confirm_just_happened: bool,
    pub browser_just_closed: bool,
    pub force_redraw: bool,
}

impl CluuampModel {
    pub fn new(playlist: Vec<String>, width: usize, height: usize) -> Self {
        let (width, height) = crate::terminal::ensure_terminal_size();
        let mut model = Self {
            audio: AudioEngine::new(playlist),
            fft: SpectrumAnalyzer::new(),
            scope: Oscilloscope::new(),
            vis_mode: VisMode::Spectrum,
            focus: FocusArea::Playlist,
            eq_enabled: false,
            show_eq: true,
            show_playlist: true,
            eq_bands: [0i8; 11],
            eq_selected: 0,
            shuffle: false,
            repeat: false,
            title_scroll_offset: 0,
            last_rendered_scroll: 0,
            scroll_accumulator: 0,
            playlist_scroll: 0,
            playlist_selected: 0,
            layout: Layout::calculate(width, height, true, true),
            transport_selected: 1,
            should_quit: false,
            browser: None,
            pending_dir_list: None,
            pending_dir_import: None,
            confirm_just_happened: false,
            browser_just_closed: false,
            force_redraw: false,
        };
        model.sync_equalizer();
        model
    }

    fn sync_equalizer(&mut self) {
        self.audio.set_equalizer(self.eq_enabled, self.eq_bands);
    }

    pub fn on_resize(&mut self, width: usize, height: usize) {
        self.layout = Layout::calculate(width, height, self.show_eq, self.show_playlist);
    }

    fn recalc_layout(&mut self) {
        self.layout = Layout::calculate(
            self.layout.width,
            self.layout.height,
            self.show_eq,
            self.show_playlist,
        );
        // A show_eq/show_playlist toggle can shrink the visible region,
        // leaving stale cells from the previous layout on screen (the diff
        // renderer only overwrites cells the new frame actually draws).
        // Force a full clear + redraw, same as the resize/browser-close paths.
        self.force_redraw = true;
    }

    /// If focus sits on a window that was just hidden, move it home.
    fn fix_focus_after_toggle(&mut self) {
        if (self.focus == FocusArea::Eq && !self.show_eq)
            || (self.focus == FocusArea::Playlist && !self.show_playlist)
        {
            self.focus = FocusArea::Transport;
        }
    }

    pub fn audio_tick(&mut self) -> Result<()> {
        self.audio.tick()?;
        if self.audio.has_new_pcm() {
            self.fft.process_pcm(self.audio.pcm_mono());
            if self.vis_mode == VisMode::Oscilloscope {
                self.scope
                    .process_pcm(self.audio.pcm_scope(), self.audio.channels() as usize);
            }
            self.audio.clear_new_pcm();
        }
        self.fft.tick();
        Ok(())
    }

    pub fn ui_tick(&mut self) {
        let display = self.audio.display_title(self.audio.current_index());
        let title_len = display.chars().count();
        let marquee_width = self.layout.marquee_width;
        if title_len > marquee_width {
            self.scroll_accumulator += 1;
            if self.scroll_accumulator >= 15 {
                self.scroll_accumulator = 0;
                self.title_scroll_offset = (self.title_scroll_offset + 1) % (title_len + 7);
            }
        } else {
            self.title_scroll_offset = 0;
            self.scroll_accumulator = 0;
        }
        if self.audio.state() == PlaybackState::Stopped
            && self.playlist_selected != self.audio.current_index()
        {
            self.playlist_selected = self.audio.current_index();
        }
        self.clamp_playlist_scroll();
    }

    fn clamp_playlist_scroll(&mut self) {
        let pl = self.audio.playlist();
        let visible = self.layout.playlist_height;
        if pl.is_empty() {
            self.playlist_scroll = 0;
            return;
        }
        let max_scroll = pl.len().saturating_sub(visible);
        if self.playlist_scroll > max_scroll {
            self.playlist_scroll = max_scroll;
        }
        if self.playlist_selected < self.playlist_scroll {
            self.playlist_scroll = self.playlist_selected;
        }
        if self.playlist_selected >= self.playlist_scroll + visible {
            self.playlist_scroll = self.playlist_selected - visible + 1;
        }
    }

    /// True when the marquee scroll offset advanced since the last render.
    /// Callers must invoke `mark_rendered()` after drawing to re-arm.
    pub fn title_scroll_changed(&self) -> bool {
        self.title_scroll_offset != self.last_rendered_scroll
    }

    /// Latch the current scroll offset as "rendered". Called by the main
    /// loop after view::render completes.
    pub fn mark_title_rendered(&mut self) {
        self.last_rendered_scroll = self.title_scroll_offset;
    }

    /// Whether the current title is long enough to require marquee scrolling.
    /// Used by the main loop to decide tick cadence when paused — a scrolling
    /// marquee needs ~13ms ticks to stay smooth even with audio stopped.
    pub fn title_is_scrolling(&self) -> bool {
        let display = self.audio.display_title(self.audio.current_index());
        display.chars().count() > self.layout.marquee_width
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if let KeyEvent::Ctrl('c') = key {
            self.should_quit = true;
            return;
        }
        if self.browser.is_some() {
            self.handle_browser_key(key);
            return;
        }
        self.force_redraw = true;
        match key {
            KeyEvent::Ctrl('c') | KeyEvent::Char('q') => {
                self.should_quit = true;
            }
            KeyEvent::Esc => {
                self.should_quit = true;
            }
            KeyEvent::Char(' ') => {
                match self.audio.state() {
                    PlaybackState::Playing => self.audio.pause(),
                    PlaybackState::Paused => {
                        let _ = self.audio.play();
                    }
                    PlaybackState::Stopped => {
                        let _ = self.audio.play();
                    }
                }
            }
            KeyEvent::Char('n') => {
                let _ = self.audio.next();
            }
            KeyEvent::Char('b') => {
                let _ = self.audio.prev();
            }
            KeyEvent::Char('s') => {
                self.audio.stop();
            }
            KeyEvent::Char('v') => {
                self.vis_mode = self.vis_mode.toggle();
            }
            KeyEvent::Char('e') => {
                self.show_eq = !self.show_eq;
                self.recalc_layout();
                self.fix_focus_after_toggle();
            }
            KeyEvent::Char('E') => {
                self.eq_enabled = !self.eq_enabled;
                self.sync_equalizer();
            }
            KeyEvent::Char('p') => {
                self.show_playlist = !self.show_playlist;
                self.recalc_layout();
                self.fix_focus_after_toggle();
            }
            KeyEvent::Char('r') => {
                self.audio.remove_track(self.playlist_selected);
                let len = self.audio.playlist().len();
                if len == 0 {
                    self.playlist_selected = 0;
                } else if self.playlist_selected >= len {
                    self.playlist_selected = len - 1;
                }
            }
            KeyEvent::Char('o') => {
                self.open_browser("/host");
            }
            KeyEvent::Tab => {
                self.focus = self.focus.next(self.show_eq, self.show_playlist);
            }
            KeyEvent::ShiftTab => {
                self.focus = self.focus.prev(self.show_eq, self.show_playlist);
            }
            KeyEvent::Arrow(libtui::input::Direction::Left) => self.handle_left(),
            KeyEvent::Arrow(libtui::input::Direction::Right) => self.handle_right(),
            KeyEvent::Arrow(libtui::input::Direction::Up) => self.handle_up(),
            KeyEvent::Arrow(libtui::input::Direction::Down) => self.handle_down(),
            KeyEvent::Enter => self.handle_enter(),
            _ => {}
        }
    }

    pub fn open_browser(&mut self, initial_dir: &str) {
        let page_size = self.layout.playlist_height.saturating_sub(2).max(5);
        let dialog = FileDialog::open_multi(initial_dir, page_size);
        self.pending_dir_list = Some(String::from(initial_dir));
        self.browser = Some(dialog);
        self.force_redraw = true;
    }

    fn handle_browser_key(&mut self, key: KeyEvent) {
        let dialog = self.browser.as_mut().unwrap();
        let action = dialog.handle_key(key);
        match action {
            DialogAction::None => {}
            DialogAction::Cancel => {
                self.browser = None;
                self.pending_dir_list = None;
                self.browser_just_closed = true;
            }
            DialogAction::EnterDir(path) => {
                if let Some(d) = self.browser.as_mut() {
                    d.set_cwd(&path);
                }
                self.pending_dir_list = Some(path);
            }
            DialogAction::Open(paths) => {
                self.audio.extend_playlist(paths);
                self.browser = None;
                self.pending_dir_list = None;
                self.confirm_just_happened = true;
                self.browser_just_closed = true;
            }
            DialogAction::OpenDir(path) => {
                self.pending_dir_list = Some(path.clone());
                self.pending_dir_import = Some(path);
                self.browser = None;
                self.browser_just_closed = true;
            }
            _ => {}
        }
    }

    pub fn take_pending_dir_list(&mut self) -> Option<String> {
        self.pending_dir_list.take()
    }

    pub fn take_pending_dir_import(&mut self) -> Option<String> {
        self.pending_dir_import.take()
    }

    pub fn browser_listed(&mut self, entries: Vec<DirEntry>) {
        if let Some(d) = self.browser.as_mut() {
            d.set_entries(entries);
        }
        self.pending_dir_list = None;
        self.force_redraw = true;
    }

    pub fn browser_active(&self) -> bool {
        self.browser.is_some()
    }

    pub fn play(&mut self) -> Result<()> {
        self.audio.play()
    }

    fn handle_left(&mut self) {
        match self.focus {
            FocusArea::Transport => {
                if self.transport_selected > 0 {
                    self.transport_selected -= 1;
                }
            }
            FocusArea::Volume => {
                self.audio.set_volume(self.audio.volume().saturating_sub(5));
            }
            FocusArea::Balance => {
                self.audio.set_balance(self.audio.balance().saturating_sub(5));
            }
            FocusArea::Position => {
                // seek backward — not implemented (no seek API in virtio-snd)
            }
            FocusArea::Eq => {
                if self.eq_selected > 0 {
                    self.eq_selected -= 1;
                }
            }
            FocusArea::Playlist => {
                // no horizontal action
            }
        }
    }

    fn handle_right(&mut self) {
        match self.focus {
            FocusArea::Transport => {
                if self.transport_selected < 7 {
                    self.transport_selected += 1;
                }
            }
            FocusArea::Volume => {
                let v = self.audio.volume();
                self.audio.set_volume(v.saturating_add(5).min(100));
            }
            FocusArea::Balance => {
                let b = self.audio.balance();
                self.audio.set_balance(b.saturating_add(5).min(50));
            }
            FocusArea::Position => {
                // seek forward — not implemented
            }
            FocusArea::Eq => {
                if self.eq_selected < 10 {
                    self.eq_selected += 1;
                }
            }
            FocusArea::Playlist => {}
        }
    }

    fn handle_up(&mut self) {
        match self.focus {
            FocusArea::Eq => {
                if self.eq_selected < 11 {
                    self.eq_bands[self.eq_selected] =
                        (self.eq_bands[self.eq_selected] + 1).min(12);
                    self.sync_equalizer();
                }
            }
            FocusArea::Playlist => {
                if self.playlist_selected > 0 {
                    self.playlist_selected -= 1;
                }
            }
            _ => {
                if self.focus == FocusArea::Volume {
                    let v = self.audio.volume();
                    self.audio.set_volume(v.saturating_add(5).min(100));
                }
            }
        }
    }

    fn handle_down(&mut self) {
        match self.focus {
            FocusArea::Eq => {
                if self.eq_selected < 11 {
                    self.eq_bands[self.eq_selected] =
                        (self.eq_bands[self.eq_selected] - 1).max(-12);
                    self.sync_equalizer();
                }
            }
            FocusArea::Playlist => {
                let pl = self.audio.playlist();
                if self.playlist_selected + 1 < pl.len() {
                    self.playlist_selected += 1;
                }
            }
            _ => {
                if self.focus == FocusArea::Volume {
                    self.audio.set_volume(self.audio.volume().saturating_sub(5));
                }
            }
        }
    }

    fn handle_enter(&mut self) {
        match self.focus {
            FocusArea::Transport => {
                match self.transport_selected {
                    0 => {
                        let _ = self.audio.prev();
                    }
                    1 => {
                        let _ = self.audio.play();
                    }
                    2 => self.audio.pause(),
                    3 => self.audio.stop(),
                    4 => {
                        let _ = self.audio.next();
                    }
                    5 => {
                        self.open_browser("/host");
                    }
                    6 => {
                        self.shuffle = !self.shuffle;
                    }
                    7 => {
                        self.repeat = !self.repeat;
                    }
                    _ => {}
                }
            }
            FocusArea::Playlist => {
                let _ = self.audio.select_track(self.playlist_selected);
            }
            _ => {}
        }
    }

    pub fn format_position(&self) -> alloc::string::String {
        let pos = self.audio.position_ms();
        let mins = pos / 60000;
        let secs = (pos / 1000) % 60;
        format!("{:02}:{:02}", mins, secs)
    }
}
