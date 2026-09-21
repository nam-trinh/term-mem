//! `tmem run <assistant>` — the explicitly lossy last resort.
//!
//! docs/plan.md puts this last and keeps it there, because the governing rule
//! does not move: **we never watch the terminal.** Capture happens only from
//! processes with an explicit adapter, and this file adds no exception — it
//! adds a small table of REPLs whose turn structure is understood, and refuses
//! anything not in it.
//!
//! Its constituency is the plain-REPL tier — `ollama run`, `llama.cpp -i`,
//! `sgpt --repl` — where there is genuinely nothing on disk. Every coding agent
//! surveyed so far persists a transcript, and for those this path is strictly
//! worse than the adapter that reads it.
//!
//! **The recording is a transcript, not a database write.** The session writes
//! JSONL under the data directory and the ordinary ingest path parses it
//! afterwards, which is what keeps redaction, the `forgotten` tombstone and
//! idempotency working without a third implementation of any of them.
//!
//! ## Turn boundaries come from stdin, not from the screen
//!
//! docs/tech-stack.md describes reconstructing turns "from the prompt pattern
//! the adapter declares", by parsing the repainting screen. That is the hard
//! version of this problem and it is not the one we have: because we own the
//! pty, we can see *what the user typed* separately from what the program
//! printed. A turn boundary is the user pressing Enter — no heuristic, no
//! prompt regex, no guessing which `>` was a prompt and which was quoted text.
//!
//! What stays lossy is the response: a TUI redraws, spinners emit thousands of
//! frames, and the bytes between two Enters are a *render*, not a document. The
//! ANSI parser below reconstructs a plain-text screen from them, which is much
//! better than nothing and is not the same thing as the model's actual output.

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// A REPL this wrapper knows how to sit inside.
///
/// The list is the whole allowlist. `tmem run <anything-else>` is refused,
/// which is the CLI-level expression of "we never watch the terminal".
pub struct Repl {
    pub name: &'static str,
    /// argv[0] to execute when the user does not give a full command.
    pub program: &'static str,
    pub about: &'static str,
}

pub const REPLS: &[Repl] = &[
    Repl {
        name: "ollama",
        program: "ollama",
        about: "ollama run <model>",
    },
    Repl {
        name: "llama-cli",
        program: "llama-cli",
        about: "llama.cpp's interactive CLI",
    },
    Repl {
        name: "sgpt",
        program: "sgpt",
        about: "shell-gpt, in --repl mode",
    },
];

pub fn repl_names() -> String {
    REPLS.iter().map(|r| r.name).collect::<Vec<_>>().join(", ")
}

pub fn find_repl(name: &str) -> Option<&'static Repl> {
    REPLS.iter().find(|r| r.name == name)
}

/// One recorded turn, as written to the session file.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Turn {
    pub ts_ms: i64,
    pub prompt: String,
    pub response: String,
}

/// The session header, so the parser knows what it is reading.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Header {
    pub kind: String,
    pub repl: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub session_id: String,
    pub started_ms: i64,
}

/// Strips ANSI and reconstructs plain text from a pty stream.
///
/// A real terminal emulator would keep a grid and apply cursor motion; this
/// keeps a single current line and a list of finished ones, which is enough for
/// text output and is honestly wrong for anything that repaints. The lossiness
/// is the point of this tier and is stated rather than hidden.
#[derive(Default)]
pub struct Screen {
    lines: Vec<String>,
    current: String,
    /// A carriage return that has not yet been resolved.
    ///
    /// `\r` means two different things and the difference is only visible in
    /// what comes next. Followed by `\n` it is ordinary line termination — a
    /// pty's line discipline turns every `\n` the child writes into `\r\n`, so
    /// this is the common case by a wide margin. Followed by anything else it
    /// is an overwrite, which is how a spinner animates.
    ///
    /// Treating every `\r` as an overwrite deleted the line immediately before
    /// each newline, which is to say all of them: the first live session
    /// recorded a screen of `"\n\n\n\n>>> "`.
    cr_pending: bool,
}

impl Screen {
    pub fn text(&self) -> String {
        let mut out = self.lines.join("\n");
        if !self.current.trim().is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&self.current);
        }
        out
    }

    pub fn take(&mut self) -> String {
        let t = self.text();
        self.lines.clear();
        self.current.clear();
        t
    }
}

impl vte::Perform for Screen {
    fn print(&mut self, c: char) {
        // A carriage return followed by text is an overwrite: the spinner case.
        if std::mem::take(&mut self.cr_pending) {
            self.current.clear();
        }
        self.current.push(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => {
                self.cr_pending = false;
                let line = std::mem::take(&mut self.current);
                self.lines.push(line);
            }
            b'\r' => self.cr_pending = true,
            0x08 => {
                self.cr_pending = false;
                self.current.pop();
            }
            b'\t' => self.current.push(' '),
            _ => {}
        }
    }
}

/// Everything the threads share.
///
/// The turn boundary is *not* "the user pressed Enter again", which was the
/// first design and is wrong: input and output arrive on different schedules,
/// so a paste — or any piped stdin — delivers both lines before the program has
/// printed a word, and every response is attributed to the question after it.
/// The test that caught this recorded zero turns from a two-line session.
///
/// What actually ends a turn is **the output going quiet**. Enter enqueues a
/// question; a question becomes pending when the previous one's answer has
/// stopped arriving. That is the same signal a human reads off the screen.
struct Recorder {
    screen: Screen,
    turns: usize,
    /// How many lines the user sent. Compared against `turns` at the end,
    /// because this tier drops turns and a silent drop is the failure mode this
    /// project exists to avoid.
    asked: usize,
    /// Questions typed but not yet answered, oldest first.
    queued: std::collections::VecDeque<(String, i64)>,
    pending: Option<(String, i64)>,
    /// When the child last wrote anything, for the quiescence check.
    last_output_ms: i64,
    out: std::fs::File,
}

/// How long the output has to stay quiet before a turn is considered finished.
/// Long enough to survive a model streaming token by token with a slow first
/// byte; short enough that a fast typist is not merged into one turn.
const QUIET_MS: i64 = 300;

impl Recorder {
    fn on_user_line(&mut self, line: &str) {
        let typed = line.trim().to_string();
        if typed.is_empty() {
            return;
        }
        self.asked += 1;
        self.queued.push_back((typed, crate::capture::now_ms()));
    }

    fn on_output(&mut self) {
        self.last_output_ms = crate::capture::now_ms();
    }

    /// Called on a timer. Closes a finished turn and promotes the next
    /// question, if the output has been quiet long enough to believe it.
    fn tick(&mut self) -> Result<()> {
        if self.pending.is_none() {
            if let Some((q, ts)) = self.queued.pop_front() {
                // Discard the REPL's banner — but only when it *is* a banner.
                // Everything printed before the question was typed belongs to
                // before; anything printed after it is already the answer. With
                // piped or pasted input the answer arrives within a millisecond
                // of the question, and clearing unconditionally threw it away,
                // which is how this recorded zero turns from a two-turn session.
                if self.last_output_ms < ts {
                    self.screen.take();
                }
                self.pending = Some((q, ts));
            }
            return Ok(());
        }
        let quiet = crate::capture::now_ms() - self.last_output_ms >= QUIET_MS;
        if !quiet {
            return Ok(());
        }
        // Nothing has been printed yet: the answer has not started, so keep
        // waiting rather than recording an empty one.
        if self.screen.text().trim().is_empty() {
            return Ok(());
        }
        // A turn only closes once there is another question waiting, or the
        // session ends — otherwise a pause mid-answer would cut it in half.
        if self.queued.is_empty() {
            return Ok(());
        }
        self.close_turn()?;
        if let Some(next) = self.queued.pop_front() {
            self.pending = Some(next);
        }
        Ok(())
    }

    fn close_turn(&mut self) -> Result<()> {
        let Some((prompt, ts)) = self.pending.take() else {
            return Ok(());
        };
        let raw = self.screen.take();
        let queued: Vec<String> = self.queued.iter().map(|(q, _)| q.clone()).collect();
        let response = clean_response(&raw, &prompt, &queued);
        if !prompt.is_empty() && !response.trim().is_empty() {
            let turn = Turn {
                ts_ms: ts,
                prompt,
                response,
            };
            writeln!(self.out, "{}", serde_json::to_string(&turn)?)?;
            self.out.flush()?;
            self.turns += 1;
        }
        Ok(())
    }

    /// End of session: close whatever is open, then drain anything still
    /// queued. A question typed and never answered is not an exchange, but it
    /// must not silently swallow the one before it either.
    fn finish(&mut self) -> Result<()> {
        if self.pending.is_none() {
            if let Some((q, ts)) = self.queued.pop_front() {
                if self.last_output_ms < ts {
                    self.screen.take();
                }
                self.pending = Some((q, ts));
            }
        }
        self.close_turn()?;
        self.out.flush()?;
        Ok(())
    }
}

/// Remove the terminal's echo of what the user typed, and the REPL's own
/// prompt decorations, from the front of a response.
///
/// The pty echoes input back as output, so the first line of every "response"
/// is the question. Leaving it in doubles every prompt in the archive.
///
/// `also_typed` is the rest of what the user has sent but the REPL has not
/// answered yet. It matters because the echo does not wait for the answer: a
/// paste, or piped stdin, puts *every* queued line on screen before the first
/// response arrives, so the next question shows up inside the previous
/// question's answer. Interactive typing never produces this, which is exactly
/// why it survived until a test piped two lines at once.
pub fn clean_response(raw: &str, prompt: &str, also_typed: &[String]) -> String {
    let is_echo = |line: &str| {
        let stripped = line.trim().trim_start_matches(['>', '#', '$', '?']).trim();
        stripped == prompt.trim() || also_typed.iter().any(|t| t.trim() == stripped)
    };
    let mut lines: Vec<&str> = raw.lines().collect();
    // Echoes cluster at the front, but a queued line can sit behind a blank
    // one, so the scan continues past anything empty rather than stopping.
    while let Some(first) = lines.first() {
        let f = first.trim();
        if f.is_empty() || is_echo(f) {
            lines.remove(0);
            continue;
        }
        break;
    }
    // A prompt marker left glued to the first real line of the answer.
    if let Some(first) = lines.first_mut() {
        let t = first.trim_start();
        if let Some(rest) = t.strip_prefix(">>>").or_else(|| t.strip_prefix('>')) {
            if !rest.trim().is_empty() {
                *first = rest.trim_start();
            }
        }
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    // A trailing bare prompt marker is the REPL asking for the next question.
    if lines
        .last()
        .is_some_and(|l| l.trim().trim_end_matches(['>', '#', '$']).trim().is_empty())
    {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}

/// Run the REPL under a pty, recording turns to a session file.
///
/// Returns the path written, so the caller can ingest it.
pub fn run(repl: &Repl, args: &[String]) -> Result<std::path::PathBuf> {
    use portable_pty::{CommandBuilder, PtySize};

    let dir = crate::paths::pty_sessions_dir()?;
    std::fs::create_dir_all(&dir)?;
    let session_id = ulid::Ulid::from_parts(
        crate::capture::now_ms().max(0) as u64,
        crate::capture::rand_u128(),
    )
    .to_string();
    let path = dir.join(format!("{session_id}.jsonl"));
    let mut file = std::fs::File::create(&path)?;

    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let mut argv = vec![repl.program.to_string()];
    argv.extend(args.iter().cloned());
    writeln!(
        file,
        "{}",
        serde_json::to_string(&Header {
            kind: "tmem-pty".into(),
            repl: repl.name.into(),
            argv: argv.clone(),
            cwd,
            session_id,
            started_ms: crate::capture::now_ms(),
        })?
    )?;
    file.flush()?;

    let pty = portable_pty::native_pty_system();
    let size = terminal_size();
    let pair = pty
        .openpty(PtySize {
            rows: size.0,
            cols: size.1,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("allocating a pty")?;

    let mut cmd = CommandBuilder::new(repl.program);
    for a in args {
        cmd.arg(a);
    }
    cmd.cwd(std::env::current_dir()?);
    // The child must not think it is being recorded into a pipe.
    cmd.env(
        "TERM",
        std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
    );
    // …and it must not capture *us*: a nested assistant writing its own
    // transcript would be captured twice, once by its adapter and once here.
    cmd.env("TMEM", "0");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("starting `{}`", repl.program))?;
    drop(pair.slave);

    let recorder = Arc::new(Mutex::new(Recorder {
        screen: Screen::default(),
        turns: 0,
        asked: 0,
        queued: std::collections::VecDeque::new(),
        pending: None,
        last_output_ms: crate::capture::now_ms(),
        out: file,
    }));

    // Reader: pty -> our stdout, teed through the ANSI parser.
    let mut reader = pair.master.try_clone_reader()?;
    let rec = Arc::clone(&recorder);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let reader_thread = std::thread::spawn(move || {
        let mut parser = vte::Parser::new();
        let mut buf = [0u8; 8192];
        let mut stdout = std::io::stdout();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = stdout.write_all(&buf[..n]);
                    let _ = stdout.flush();
                    if let Ok(mut r) = rec.lock() {
                        let mut screen = std::mem::take(&mut r.screen);
                        parser.advance(&mut screen, &buf[..n]);
                        r.screen = screen;
                        r.on_output();
                    }
                }
            }
        }
        let _ = done_tx.send(());
    });

    // Writer: our stdin -> pty, watching for the Enter that ends a turn.
    let mut writer = pair.master.take_writer()?;
    let rec2 = Arc::clone(&recorder);
    let _raw = RawMode::enable();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        let mut line = String::new();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                    for &b in &buf[..n] {
                        match b {
                            b'\r' | b'\n' => {
                                if let Ok(mut r) = rec2.lock() {
                                    r.on_user_line(&line);
                                }
                                line.clear();
                            }
                            0x7f | 0x08 => {
                                line.pop();
                            }
                            // Control characters are not part of a question.
                            0..=0x1f => {}
                            _ => line.push(b as char),
                        }
                    }
                }
            }
        }
    });

    // The quiescence timer. Cheap, and the only thing that decides where one
    // turn ends and the next begins.
    let rec3 = Arc::clone(&recorder);
    let ticking = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let stop = Arc::clone(&ticking);
    let ticker = std::thread::spawn(move || {
        while stop.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if let Ok(mut r) = rec3.lock() {
                let _ = r.tick();
            }
        }
    });

    let status = child.wait()?;
    let _ = done_rx.recv_timeout(std::time::Duration::from_millis(500));
    drop(pair.master);
    let _ = reader_thread.join();

    ticking.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = ticker.join();
    if let Ok(mut r) = recorder.lock() {
        r.finish()?;
    }
    let (turns, asked) = recorder
        .lock()
        .map(|r| (r.turns, r.asked))
        .unwrap_or((0, 0));
    drop(_raw);

    eprintln!(
        "\ntmem: recorded {turns} turn(s) from `{}` — this tier is lossy; see `tmem show`",
        argv.join(" ")
    );
    // Never silent about the gap. Turn boundaries here come from the output
    // going quiet, and a REPL answering faster than the quiet window merges two
    // answers into one — which is what piping a script of questions into one
    // does. A human typing and waiting does not hit it.
    if asked > turns {
        eprintln!(
            "tmem: {} line(s) you sent produced no separate exchange. That happens when \n                   answers arrive faster than the turn detector can separate them — piped or \n                   pasted input, usually. The full session is at {}",
            asked - turns,
            path.display()
        );
    }
    if !status.success() {
        eprintln!("tmem: `{}` exited with {status:?}", repl.program);
    }
    Ok(path)
}

/// Put the real terminal in raw mode so the child sees keystrokes as they are
/// typed, and put it back on the way out however we leave.
struct RawMode(Option<libc_termios::Saved>);

impl RawMode {
    fn enable() -> RawMode {
        RawMode(libc_termios::enable_raw())
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(s) = self.0.take() {
            libc_termios::restore(s);
        }
    }
}

fn terminal_size() -> (u16, u16) {
    libc_termios::size().unwrap_or((24, 80))
}

/// The three termios calls this needs, rather than a dependency for them.
mod libc_termios {
    pub struct Saved(pub nix_like::Termios);

    pub mod nix_like {
        //! `portable-pty` already brings `nix` in, so these are its types.
        pub use nix::sys::termios::Termios;
    }

    pub fn enable_raw() -> Option<Saved> {
        use nix::sys::termios::{tcgetattr, tcsetattr, SetArg};
        use std::io::IsTerminal;
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            return None; // piped input: nothing to put into raw mode
        }
        let saved = tcgetattr(&stdin).ok()?;
        let mut raw = saved.clone();
        nix::sys::termios::cfmakeraw(&mut raw);
        tcsetattr(&stdin, SetArg::TCSANOW, &raw).ok()?;
        Some(Saved(saved))
    }

    pub fn restore(s: Saved) {
        use nix::sys::termios::{tcsetattr, SetArg};
        let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &s.0);
    }

    pub fn size() -> Option<(u16, u16)> {
        use std::io::IsTerminal;
        if !std::io::stdout().is_terminal() {
            return None;
        }
        // SAFETY: TIOCGWINSZ on a fd we own, writing into a struct we own.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 {
                return Some((ws.ws_row, ws.ws_col));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(bytes: &[u8]) -> String {
        let mut s = Screen::default();
        let mut p = vte::Parser::new();
        p.advance(&mut s, bytes);
        s.text()
    }

    /// The whole job of the ANSI layer: colour codes are not content.
    #[test]
    fn escape_sequences_do_not_reach_the_archive() {
        let out = render(b"\x1b[1;32mhello\x1b[0m world\n");
        assert_eq!(out, "hello world");
        assert!(!out.contains('\x1b'));
    }

    /// A spinner emits thousands of frames separated by carriage returns. What
    /// belongs in the archive is the last one, not a flipbook.
    #[test]
    fn a_spinner_collapses_to_its_final_frame() {
        let out = render(b"| thinking\r/ thinking\r- thinking\rdone: 42\n");
        assert_eq!(out, "done: 42");
    }

    /// …but a pty's line discipline turns every `\n` into `\r\n`, so the same
    /// byte is also ordinary line termination. Treating them alike deleted the
    /// line before every newline, which is all of them: the first live session
    /// recorded `"\n\n\n\n>>> "` and nothing else.
    #[test]
    fn crlf_is_a_line_break_and_not_an_overwrite() {
        assert_eq!(render(b"first\r\nsecond\r\n"), "first\nsecond");
        assert_eq!(
            render(b"Answer: use -c copy\r\n>>> "),
            "Answer: use -c copy\n>>> "
        );
        // Both meanings in one stream.
        assert_eq!(render(b"working\rdone\r\nnext line\r\n"), "done\nnext line");
    }

    #[test]
    fn backspace_deletes_rather_than_printing() {
        assert_eq!(render(b"abcX\x08\x08cd\n"), "abcd");
    }

    /// The pty echoes input back, so the first line of a "response" is almost
    /// always the question. Leaving it in doubles every prompt in the archive.
    #[test]
    fn the_terminal_echo_of_the_question_is_not_part_of_the_answer() {
        let raw = ">>> what is a pty\nA pseudoterminal is a pair of devices.\n>>> ";
        assert_eq!(
            clean_response(raw, "what is a pty", &[]),
            "A pseudoterminal is a pair of devices."
        );
    }

    #[test]
    fn a_response_that_is_only_an_echo_is_empty() {
        assert_eq!(clean_response(">>> hello\n>>> ", "hello", &[]), "");
    }

    /// Piped or pasted input echoes every queued line before the first answer
    /// arrives, so the *next* question turns up inside this one's response.
    /// Interactive typing never does this, which is why it went unnoticed until
    /// a test sent two lines at once.
    #[test]
    fn a_queued_question_is_not_mistaken_for_part_of_the_answer() {
        let raw = "bye\n\n>>> Answer: joining mp4 files is handled by the concat demuxer.";
        assert_eq!(
            clean_response(raw, "joining mp4 files", &["bye".to_string()]),
            "Answer: joining mp4 files is handled by the concat demuxer."
        );
    }

    /// The allowlist *is* the policy: docs/plan.md's rule is that capture
    /// happens only from processes with an explicit adapter, and `tmem run`
    /// must not become a way around it.
    #[test]
    fn only_known_repls_are_accepted() {
        assert!(find_repl("ollama").is_some());
        assert!(find_repl("sgpt").is_some());
        for forbidden in ["bash", "zsh", "sh", "vim", "ssh", "psql"] {
            assert!(find_repl(forbidden).is_none(), "{forbidden} was accepted");
        }
    }
}
