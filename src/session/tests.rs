#[test]
fn exec_line_reads_alike_in_every_shell() {
    let line = exec_line("/usr/bin/fish").unwrap();
    // The assignment is quoted whole when the path has a space (macOS's
    // "Application Support"): still one word to env.
    let assignment = line.strip_prefix("exec env ").unwrap_or_default().trim_start_matches('\'');
    assert!(assignment.starts_with("TEMINO_INTEGRATION="), "{line}");
    assert!(line.ends_with("/usr/bin/fish --login --interactive --init-command 'source $TEMINO_INTEGRATION'"));
    let line = exec_line("/bin/bash").unwrap();
    assert!(line.starts_with("exec env /bin/bash --rcfile "), "{line}");
}
use super::*;

/// fish 4.9 redrawing an unchanged prompt (here after SIGWINCH): its own
/// start mark comes last, after the prompt's end.
const FISH_REPAINT: &[u8] = b"\x1b[m\x1b[?2004h\r\r\x1b]133;A;click_events=1\x1b\\\x1b]133;B\x07\x1b]133;B\x1b\\\x1b[J\x1b]133;A;click_events=1\x1b\\";

#[test]
fn prompt_reports_the_shell_path_and_notifications() {
    let theme = Theme::dark();
    let mut session = Session::new(GridSize { columns: 80, lines: 24 });
    session.feed(b"\x1b]6973;path;/opt/homebrew/bin:/usr/bin\x07\x1b]133;A\x07", &theme);
    assert_eq!(session.path.as_deref(), Some("/opt/homebrew/bin:/usr/bin"));
    let notices = session.feed(b"\x1b]9;Claude needs you\x07\x1b]9;4;3;\x07\x1b]9;4;0;\x07", &theme);
    let kinds: Vec<_> = notices
        .iter()
        .map(|n| match n {
            Notice::Notify(None, body) if body == "Claude needs you" => "notify",
            Notice::Progress(true) => "busy",
            Notice::Progress(false) => "idle",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["notify", "busy", "idle"]);
    let mut said = |bytes: &[u8]| match session.feed(bytes, &theme).pop() {
        Some(Notice::Notify(title, body)) => (title, body),
        _ => panic!("no notification"),
    };
    assert_eq!(said(b"\x1b]777;notify;Build;done in 3s\x07"), (Some("Build".into()), "done in 3s".into()));
    assert_eq!(said(b"\x1b]99;i=1:d=0;Tests passed\x07"), (None, "Tests passed".into()));
    assert_eq!(said(b"\x1b]99;i=1:p=title;Deploy\x07"), (Some("Deploy".into()), String::new()));
}

#[test]
fn urls_are_found_without_trailing_punctuation() {
    let find = |text: &str| -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find_urls(&chars).into_iter().map(|(a, b)| chars[a..b].iter().collect()).collect()
    };
    assert_eq!(find("see https://example.com/a?b=1."), ["https://example.com/a?b=1"]);
    assert_eq!(
        find("(at https://en.wikipedia.org/wiki/Rust_(language))"),
        ["https://en.wikipedia.org/wiki/Rust_(language)"]
    );
    assert_eq!(find("[docs](http://x.io/y) │ file:///tmp/a b"), ["http://x.io/y", "file:///tmp/a"]);
    assert_eq!(find("nothttps://x.io https:// "), Vec::<String>::new());
}

#[test]
fn paths_are_cut_from_their_surroundings() {
    let path = |text: &str, at: usize| -> Option<String> {
        let chars: Vec<char> = text.chars().collect();
        find_path(&chars, at).map(|(a, b)| chars[a..b].iter().collect())
    };
    assert_eq!(path("error at src/pane.rs:12:5: oops", 10).as_deref(), Some("src/pane.rs"));
    assert_eq!(path("open '~/notes.md'.", 8).as_deref(), Some("~/notes.md"));
    assert_eq!(path("│ /tmp/x │", 3).as_deref(), Some("/tmp/x"));
    assert_eq!(path("a  b", 1), None);
}

#[test]
fn local_files_are_links() {
    let theme = Theme::dark();
    let mut session = Session::new(GridSize { columns: 40, lines: 10 });
    session.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07", &theme);
    session.submit("ls");
    session.feed(b"\x1b]133;C\x07Cargo.toml  no-such-file  /tmp\r\n", &theme);
    let block = session.blocks.last_mut().unwrap();
    block.cwd = env!("CARGO_MANIFEST_DIR").to_owned();
    let link = block.link_at(0, 2).expect("Cargo.toml exists");
    assert_eq!(link.url, format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR")));
    assert_eq!(link.cells, [(0, 0, 9)]);
    assert_eq!(block.link_at(0, 14), None);
    assert_eq!(block.link_at(0, 27).map(|l| l.url).as_deref(), Some("/tmp"));
}

#[test]
fn links_follow_soft_wraps() {
    let theme = Theme::dark();
    let mut session = Session::new(GridSize { columns: 20, lines: 10 });
    session.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07", &theme);
    session.submit("echo");
    session.feed(b"\x1b]133;C\x07go to https://example.com/very/long/path now\r\n", &theme);
    let block = session.blocks.last().unwrap();
    let link = block.link_at(1, 3).expect("a link on the wrapped line");
    assert_eq!(link.url, "https://example.com/very/long/path");
    assert_eq!(link.cells, [(0, 6, 19), (1, 0, 19)]);
    assert_eq!(block.link_at(0, 1), None);
    session.feed(b"\x1b]8;;https://anthropic.com\x07here\x1b]8;;\x07\r\n", &theme);
    let block = session.blocks.last().unwrap();
    let link = block.link_at(3, 2).expect("an OSC 8 link");
    assert_eq!((link.url.as_str(), link.cells.as_slice()), ("https://anthropic.com", &[(3, 0, 3)][..]));
}

#[test]
fn repaint_keeps_the_prompt() {
    let theme = Theme::dark();
    let mut session = Session::new(GridSize { columns: 80, lines: 24 });
    session.feed(b"\x1b]133;A\x07\x1b]133;B\x07", &theme);
    assert_eq!(session.phase, Phase::Ready);
    session.feed(FISH_REPAINT, &theme);
    assert_eq!(session.phase, Phase::Ready);
    session.tick(Instant::now() + IDLE_OUTPUT_DELAY);
    assert_eq!(session.deadline(), None);
}

#[test]
fn saved_blocks_come_back_with_their_colors() {
    let size = GridSize { lines: 10, columns: 40 };
    let mut session = Session::new(size);
    let block = session.new_block(BlockKind::Command, "echo 'a\\b'\nls".into());
    block.feed(b"\x1b[31mred\x1b[0m plain\r\nline two");
    block.exit = Some(2);
    block.finished = Some(Instant::now());
    let path = std::env::temp_dir().join(format!("temino-restore-{}", std::process::id()));
    let path = path.to_str().unwrap();
    session.save_blocks(path);
    let mut restored = Session::new(size);
    restored.restore_blocks(path);
    let _ = std::fs::remove_file(path);
    let block = &mut restored.blocks[0];
    assert_eq!(block.command, "echo 'a\\b'\nls");
    assert_eq!(block.exit, Some(2));
    assert_eq!(block.output_text(), "red plain\nline two");
    let red = &block.term.grid()[block.grid_line(0)][Column(0)];
    assert_eq!(red.fg, alacritty_terminal::vte::ansi::Color::Named(alacritty_terminal::vte::ansi::NamedColor::Red));
}

#[test]
fn stale_timers_never_wake() {
    let theme = Theme::dark();
    let mut session = Session::new(GridSize { columns: 80, lines: 24 });
    session.feed(b"\x1b]133;A\x07\x1b]133;B\x07", &theme);
    session.feed(b"background job output\r\n", &theme);
    session.phase = Phase::Prompt;
    assert_eq!(session.deadline(), None);
}
