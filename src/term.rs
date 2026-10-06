//! Plain-terminal output following ~/git/tui-guideline: palette colors only,
//! single-letter keys read in raw mode, and a two-line live status (status +
//! key hints) redrawn in place. No full-screen UI.

use std::io::{self, BufRead, Write};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::sys::termios::{LocalFlags, SetArg, SpecialCharacterIndices, Termios, tcgetattr, tcsetattr};

// ── signals ────────────────────────────────────────────────────────────────

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// SIGINT/SIGTERM/SIGHUP ask the recording to stop and finalize.
pub fn install_signal_handlers() {
    let action = SigAction::new(SigHandler::Handler(on_signal), SaFlags::empty(), SigSet::empty());
    for sig in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        // SAFETY: the handler only stores to an atomic.
        let _ = unsafe { sigaction(sig, &action) };
    }
}

pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

// ── terminal facts ─────────────────────────────────────────────────────────

pub fn stdout_is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

pub fn stdin_is_tty() -> bool {
    // SAFETY: as above.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

pub fn width() -> usize {
    // SAFETY: TIOCGWINSZ fills a winsize struct and nothing else.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 { ws.ws_col as usize } else { 80 }
}

/// Print a line, ignoring a closed stdout (the terminal may be gone on SIGHUP).
pub fn say(line: impl AsRef<str>) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{}", line.as_ref());
    let _ = out.flush();
}

// ── styles ─────────────────────────────────────────────────────────────────

/// One meaning per color (guideline §5). Colors come from the terminal
/// palette: slot 16 is the accent (peach in the Catppuccin foot theme);
/// `RCRD_ACCENT` overrides it with an ANSI color name elsewhere.
#[derive(Clone)]
pub struct Style {
    color: bool,
    accent: String,
}

const RESET: &str = "\x1b[0m";

impl Style {
    pub fn detect() -> Self {
        let color = stdout_is_tty() && std::env::var_os("NO_COLOR").is_none();
        let accent = match std::env::var("RCRD_ACCENT").ok().as_deref() {
            Some("red") => "31".into(),
            Some("green") => "32".into(),
            Some("yellow") => "33".into(),
            Some("blue") => "34".into(),
            Some("magenta") => "35".into(),
            Some("cyan") => "36".into(),
            Some("white") => "37".into(),
            Some(n) if n.parse::<u8>().is_ok() => format!("38;5;{n}"),
            _ => "38;5;16".into(),
        };
        Self { color, accent }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color { format!("\x1b[{code}m{s}{RESET}") } else { s.to_string() }
    }

    /// The tool itself and live activity: `●`, `❯`, key chips.
    pub fn accent(&self, s: &str) -> String {
        self.paint(&self.accent, s)
    }
    /// In progress.
    pub fn accent2(&self, s: &str) -> String {
        self.paint("34", s)
    }
    pub fn ok(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn err(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn warn(&self, s: &str) -> String {
        self.paint("33", s)
    }
    /// Secondary text.
    pub fn dim(&self, s: &str) -> String {
        self.paint("97", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }

    /// `key label` chips joined by two spaces.
    pub fn chips(&self, chips: &[(&str, &str)]) -> String {
        chips
            .iter()
            .map(|(k, l)| format!("{} {}", self.accent(k), self.dim(l)))
            .collect::<Vec<_>>()
            .join("  ")
    }
}

/// Characters shown on screen, ignoring SGR escape sequences.
pub fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            if c == 'm' {
                in_esc = false;
            }
        } else if c == '\x1b' {
            in_esc = true;
        } else {
            n += 1;
        }
    }
    n
}

/// Keep the line within `width` columns, ending with `…` when cut.
pub fn truncate(s: &str, width: usize) -> String {
    if visible_len(s) <= width || width == 0 {
        return s.to_string();
    }
    let mut out = String::new();
    let mut shown = 0;
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            out.push(c);
            if c == 'm' {
                in_esc = false;
            }
        } else if c == '\x1b' {
            in_esc = true;
            out.push(c);
        } else if shown + 1 < width {
            out.push(c);
            shown += 1;
        } else {
            break;
        }
    }
    out.push('…');
    out.push_str(RESET);
    out
}

// ── keys ───────────────────────────────────────────────────────────────────

/// Restores the terminal on drop (also on panic or an early return).
pub struct RawMode {
    saved: Termios,
}

impl RawMode {
    /// Cbreak-like mode: no echo, no line buffering, no signal keys (Ctrl-C
    /// arrives as a byte and is handled like `q`).
    pub fn enable() -> Option<Self> {
        let stdin = io::stdin();
        let saved = tcgetattr(stdin.as_fd()).ok()?;
        let mut raw = saved.clone();
        raw.local_flags &=
            !(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG | LocalFlags::IEXTEN);
        raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
        raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
        tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &raw).ok()?;
        Some(Self { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = tcsetattr(io::stdin().as_fd(), SetArg::TCSANOW, &self.saved);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Quit,
    Mute,
    Marker,
    Start,
}

/// Wait up to `timeout` for a key. Escape sequences (arrows…) are ignored.
pub fn read_key(timeout: Duration) -> Option<Key> {
    let stdin = io::stdin();
    let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
    let ms = timeout.as_millis().min(u128::from(u16::MAX)) as u16;
    match poll(&mut fds, PollTimeout::from(ms)) {
        Ok(n) if n > 0 => {}
        _ => return None,
    }
    let mut buf = [0u8; 16];
    let n = nix::unistd::read(libc::STDIN_FILENO, &mut buf).ok()?;
    if n == 0 {
        return None;
    }
    match buf[0] {
        b'q' | b'Q' | 0x03 => Some(Key::Quit),
        0x1b if n == 1 => Some(Key::Quit),
        0x1b => None,
        b'm' | b'M' => Some(Key::Mute),
        b'b' | b'B' => Some(Key::Marker),
        b's' | b'S' => Some(Key::Start),
        _ => None,
    }
}

// ── prompts (before the live lines start) ──────────────────────────────────

/// Numbered list + `prompt ❯` line. Enter picks `default` (or nothing when
/// there is none); `0` picks the `none_label` entry when offered.
pub fn choose(
    style: &Style,
    prompt: &str,
    items: &[String],
    default: Option<usize>,
    none_label: Option<&str>,
) -> Result<Option<usize>> {
    let width = width();
    for (i, item) in items.iter().enumerate() {
        let mark = if Some(i) == default { style.accent("❯") } else { " ".to_string() };
        say(truncate(&format!("{mark} {:>2}  {item}", i + 1), width));
    }
    if let Some(label) = none_label {
        say(format!("   0  {}", style.dim(label)));
    }
    if !stdin_is_tty() {
        return Ok(default);
    }
    loop {
        {
            let mut out = io::stdout().lock();
            let _ = write!(out, "{} {} ", style.dim(prompt), style.accent("❯"));
            let _ = out.flush();
        }
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line)? == 0 {
            return Ok(default);
        }
        let t = line.trim();
        if t.is_empty() {
            return Ok(default);
        }
        if t == "0" && none_label.is_some() {
            return Ok(None);
        }
        match t.parse::<usize>() {
            Ok(n) if (1..=items.len()).contains(&n) => return Ok(Some(n - 1)),
            _ => say(style.warn("pick a number from the list")),
        }
    }
}

// ── live lines ─────────────────────────────────────────────────────────────

/// A few lines at the bottom of the output, redrawn in place; log lines are
/// printed above them and scroll normally.
pub struct Live {
    pub enabled: bool,
    drawn: usize,
    cursor_hidden: bool,
}

impl Live {
    pub fn new(enabled: bool) -> Self {
        Self { enabled, drawn: 0, cursor_hidden: false }
    }

    fn erase(&mut self, out: &mut impl Write) {
        if self.drawn > 0 {
            let _ = write!(out, "\r");
            if self.drawn > 1 {
                let _ = write!(out, "\x1b[{}A", self.drawn - 1);
            }
            let _ = write!(out, "\x1b[J");
            self.drawn = 0;
        }
    }

    /// Print a permanent line above the live lines.
    pub fn log(&mut self, line: impl AsRef<str>) {
        let mut out = io::stdout().lock();
        self.erase(&mut out);
        let _ = writeln!(out, "{}", line.as_ref());
        let _ = out.flush();
    }

    /// Redraw the live lines (each truncated to the terminal width).
    pub fn draw(&mut self, lines: &[String]) {
        if !self.enabled {
            return;
        }
        let width = width().saturating_sub(1).max(10);
        let mut out = io::stdout().lock();
        self.erase(&mut out);
        if !self.cursor_hidden {
            let _ = write!(out, "\x1b[?25l");
            self.cursor_hidden = true;
        }
        let text = lines.iter().map(|l| truncate(l, width)).collect::<Vec<_>>().join("\n");
        let _ = write!(out, "{text}");
        let _ = out.flush();
        self.drawn = lines.len();
    }

    /// Remove the live lines and restore the cursor.
    pub fn finish(&mut self) {
        let mut out = io::stdout().lock();
        self.erase(&mut out);
        if self.cursor_hidden {
            let _ = write!(out, "\x1b[?25h");
            self.cursor_hidden = false;
        }
        let _ = out.flush();
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.finish();
    }
}
