//! Cluuamp model: playback state, UI state, focus, tick logic, key handling.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libcluu::Result;
use libtui::components::browser::{BrowserAction, FileBrowser};
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
    pub eq_bands: [i8; 11],
    pub eq_selected: usize,
    pub shuffle: bool,
    pub repeat: bool,
    pub title_scroll_offset: usize,
    pub playlist_scroll: usize,
    pub playlist_selected: usize,
    pub layout: Layout,
    pub transport_selected: usize,
    pub should_quit: bool,
    pub browser: Option<FileBrowser>,
    pub pending_dir_list: Option<String>,
    pub confirm_just_happened: bool,
    pub browser_just_closed: bool,
}

impl CluuampModel {
    pub fn new(playlist: Vec<String>, width: usize, height: usize) -> Self {
        Self {
            audio: AudioEngine::new(playlist),
            fft: SpectrumAnalyzer::new(),
            scope: Oscilloscope::new(),
            vis_mode: VisMode::Spectrum,
            focus: FocusArea::Playlist,
            eq_enabled: false,
            eq_bands: [0i8; 11],
            eq_selected: 0,
            shuffle: false,
            repeat: false,
            title_scroll_offset: 0,
            playlist_scroll: 0,
            playlist_selected: 0,
            layout: Layout::calculate(width, height),
            transport_selected: 2,
            should_quit: false,
            browser: None,
            pending_dir_list: None,
            confirm_just_happened: false,
            browser_just_closed: false,
        }
    }

    pub fn on_resize(&mut self, width: usize, height: usize) {
        self.layout = Layout::calculate(width, height);
    }

    pub fn tick(&mut self) -> Result<()> {
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
        let title = self.audio.current_title();
        let title_len = title.chars().count();
        let marquee_width = self.layout.width.saturating_sub(12);
        if title_len > marquee_width {
            self.title_scroll_offset = (self.title_scroll_offset + 1) % (title_len + 7);
        }
        if self.audio.state() == PlaybackState::Stopped
            && self.playlist_selected != self.audio.current_index()
        {
            self.playlist_selected = self.audio.current_index();
        }
        self.clamp_playlist_scroll();
        Ok(())
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

    pub fn handle_key(&mut self, key: KeyEvent) {
        if let KeyEvent::Ctrl('c') = key {
            self.should_quit = true;
            return;
        }
        if self.browser.is_some() {
            self.handle_browser_key(key);
            return;
        }
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
                self.eq_enabled = !self.eq_enabled;
            }
            KeyEvent::Char('o') => {
                self.open_browser("/host");
            }
            KeyEvent::Tab => {
                self.focus = self.focus.next();
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
        let mut b = FileBrowser::new(initial_dir, page_size, true);
        b.set_title(" Add to playlist ");
        self.pending_dir_list = Some(String::from(initial_dir));
        self.browser = Some(b);
    }

    fn handle_browser_key(&mut self, key: KeyEvent) {
        let browser = self.browser.as_mut().unwrap();
        let action = browser.handle_key(key);
        match action {
            BrowserAction::None => {}
            BrowserAction::Cancel => {
                self.browser = None;
                self.pending_dir_list = None;
                self.browser_just_closed = true;
            }
            BrowserAction::EnterDir(path) => {
                if let Some(b) = self.browser.as_mut() {
                    b.set_cwd(&path);
                }
                self.pending_dir_list = Some(path);
            }
            BrowserAction::Confirm(paths) => {
                self.audio.extend_playlist(paths);
                self.browser = None;
                self.pending_dir_list = None;
                self.confirm_just_happened = true;
                self.browser_just_closed = true;
            }
        }
    }

    pub fn take_pending_dir_list(&mut self) -> Option<String> {
        self.pending_dir_list.take()
    }

    pub fn browser_listed(&mut self, entries: Vec<libtui::components::browser::DirEntry>) {
        if let Some(b) = self.browser.as_mut() {
            b.set_entries(entries);
        }
        self.pending_dir_list = None;
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
                if self.transport_selected < 5 {
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
                    1 => self.audio.stop(),
                    2 => {
                        match self.audio.state() {
                            PlaybackState::Playing => self.audio.pause(),
                            _ => {
                                let _ = self.audio.play();
                            }
                        }
                    }
                    3 => {
                        let _ = self.audio.next();
                    }
                    4 => {
                        self.shuffle = !self.shuffle;
                    }
                    5 => {
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
