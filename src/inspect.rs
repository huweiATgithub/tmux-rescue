use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use anstyle::{AnsiColor, Style};
use termtree::{GlyphPalette, Tree};
use tmux_rescue::{
    AutomaticRecovery, CaptureConsistency, CapturedCommand, LoadedSnapshot, PaneRecovery,
};

use crate::cli::SnapshotSelection;

const TREE_GLYPHS: GlyphPalette = GlyphPalette {
    middle_item: "├",
    last_item: "└",
    item_indent: "─ ",
    middle_skip: "│",
    last_skip: " ",
    skip_indent: "  ",
};
const BOLD: Style = Style::new().bold();
const CYAN: Style = AnsiColor::Cyan.on_default();
const GREEN: Style = AnsiColor::Green.on_default();
const YELLOW: Style = AnsiColor::Yellow.on_default();
const RED: Style = AnsiColor::Red.on_default();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Palette {
    colored: bool,
}

impl Palette {
    pub(crate) const fn plain() -> Self {
        Self { colored: false }
    }

    pub(crate) const fn colored() -> Self {
        Self { colored: true }
    }

    pub(crate) fn fatal_prefix(self) -> String {
        self.paint(RED, "error:")
    }

    fn bold(self, text: &str) -> String {
        self.paint(BOLD, text)
    }

    fn cyan(self, text: &str) -> String {
        self.paint(CYAN, text)
    }

    fn green(self, text: &str) -> String {
        self.paint(GREEN, text)
    }

    fn yellow(self, text: &str) -> String {
        self.paint(YELLOW, text)
    }

    fn paint(self, style: Style, text: &str) -> String {
        if self.colored {
            format!("{}{text}{}", style.render(), style.render_reset())
        } else {
            text.to_owned()
        }
    }
}

pub(crate) fn render(
    loaded: &LoadedSnapshot,
    selection: &SnapshotSelection,
    palette: Palette,
) -> String {
    InspectView::from_loaded(loaded, selection).render(palette)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DisplayValue(String);

impl DisplayValue {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(display_bytes(bytes))
    }

    fn from_os(value: &OsStr) -> Self {
        Self::from_bytes(value.as_bytes())
    }

    fn from_str(value: &str) -> Self {
        Self::from_bytes(value.as_bytes())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Eq, PartialEq)]
struct InspectView {
    selection: &'static str,
    captured_at: DisplayValue,
    source: DisplayValue,
    consistency: ConsistencyView,
    file: DisplayValue,
    contents: Contents,
    programs: Vec<ProgramEntry>,
    sessions: Vec<SessionView>,
}

impl InspectView {
    fn from_loaded(loaded: &LoadedSnapshot, selection: &SnapshotSelection) -> Self {
        let snapshot = loaded.snapshot();
        let selection = match selection {
            SnapshotSelection::Latest => "latest",
            SnapshotSelection::Explicit(_) => "explicit",
        };
        let consistency = match snapshot.consistency() {
            CaptureConsistency::Stable => ConsistencyView::Stable,
            CaptureConsistency::Unstable { attempts } => ConsistencyView::Unstable {
                attempts: attempts.get(),
            },
        };

        let sessions = snapshot
            .sessions()
            .iter()
            .map(SessionView::from_snapshot)
            .collect::<Vec<_>>();
        let contents = Contents::from_sessions(&sessions);
        let programs = ProgramEntry::from_sessions(&sessions);

        Self {
            selection,
            captured_at: DisplayValue::from_str(snapshot.captured_at().encoded()),
            source: DisplayValue::from_bytes(snapshot.source().path().as_bytes()),
            consistency,
            file: DisplayValue::from_os(loaded.path().as_os_str()),
            contents,
            programs,
            sessions,
        }
    }

    fn render(&self, palette: Palette) -> String {
        let mut output = String::new();
        writeln!(output, "Snapshot     {}", palette.bold(self.selection)).unwrap();
        writeln!(output, "Captured     {}", self.captured_at.as_str()).unwrap();
        writeln!(output, "Source       {}", self.source.as_str()).unwrap();
        match self.consistency {
            ConsistencyView::Stable => {
                writeln!(
                    output,
                    "Consistency  {} stable topology",
                    palette.green("●")
                )
                .unwrap();
            }
            ConsistencyView::Unstable { attempts } => {
                let warning = format!("unstable topology after {attempts} attempts");
                writeln!(
                    output,
                    "Consistency  {} {}",
                    palette.yellow("▲"),
                    palette.bold(&warning),
                )
                .unwrap();
            }
        }
        writeln!(output, "File         {}", self.file.as_str()).unwrap();
        output.push('\n');
        writeln!(
            output,
            "Contents     {} {} · {} {} · {} {}",
            self.contents.sessions,
            count_noun(self.contents.sessions, "session", "sessions"),
            self.contents.windows,
            count_noun(self.contents.windows, "window", "windows"),
            self.contents.panes,
            count_noun(self.contents.panes, "pane", "panes"),
        )
        .unwrap();
        write!(output, "Programs     ").unwrap();
        for (position, program) in self.programs.iter().enumerate() {
            if position > 0 {
                output.push_str(" · ");
            }
            write!(output, "{} {}", program.count, program.visible_label()).unwrap();
        }
        output.push_str("\n\n");

        for (position, session) in self.sessions.iter().enumerate() {
            if position > 0 {
                output.push('\n');
            }
            write!(output, "{}", session.tree(palette)).unwrap();
        }
        output
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConsistencyView {
    Stable,
    Unstable { attempts: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Contents {
    sessions: usize,
    windows: usize,
    panes: usize,
}

impl Contents {
    fn from_sessions(sessions: &[SessionView]) -> Self {
        let windows = sessions.iter().map(|session| session.windows.len()).sum();
        let panes = sessions
            .iter()
            .flat_map(|session| &session.windows)
            .map(|window| window.panes.len())
            .sum();
        Self {
            sessions: sessions.len(),
            windows,
            panes,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ProgramEntry {
    identity: String,
    count: usize,
}

impl ProgramEntry {
    fn from_sessions(sessions: &[SessionView]) -> Vec<Self> {
        let mut programs: Vec<Self> = Vec::new();
        let mut indexes: HashMap<&str, usize> = HashMap::new();
        for pane in sessions
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
        {
            let identity = pane.fact.program_identity();
            if let Some(index) = indexes.get(identity).copied() {
                programs[index].count += 1;
            } else {
                indexes.insert(identity, programs.len());
                programs.push(Self {
                    identity: identity.to_owned(),
                    count: 1,
                });
            }
        }
        programs
    }

    fn visible_label(&self) -> &str {
        if self.identity == "shell" && self.count != 1 {
            "shells"
        } else {
            &self.identity
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct SessionView {
    name: DisplayValue,
    working_directory: DisplayValue,
    windows: Vec<WindowView>,
}

impl SessionView {
    fn from_snapshot(session: &tmux_rescue::SessionSnapshot) -> Self {
        let windows = session
            .windows()
            .iter()
            .map(|window| WindowView::from_snapshot(window, session.working_directory().as_bytes()))
            .collect();
        Self {
            name: DisplayValue::from_str(session.name()),
            working_directory: DisplayValue::from_bytes(session.working_directory().as_bytes()),
            windows,
        }
    }

    fn tree(&self, palette: Palette) -> Tree<String> {
        let pane_count = self
            .windows
            .iter()
            .map(|window| window.panes.len())
            .sum::<usize>();
        let root = format!(
            "{} {} · {} {} · {} {}\n  cwd {}",
            palette.cyan("◆"),
            palette.bold(self.name.as_str()),
            self.windows.len(),
            count_noun(self.windows.len(), "window", "windows"),
            pane_count,
            count_noun(pane_count, "pane", "panes"),
            self.working_directory.as_str(),
        );
        let mut tree = Tree::new(root).with_glyphs(TREE_GLYPHS);
        for window in &self.windows {
            tree.push(window.tree(palette));
        }
        tree
    }
}

#[derive(Debug, Eq, PartialEq)]
struct WindowView {
    source_index: u32,
    name: DisplayValue,
    panes: Vec<PaneView>,
}

impl WindowView {
    fn from_snapshot(window: &tmux_rescue::WindowSnapshot, session_cwd: &[u8]) -> Self {
        Self {
            source_index: window.source_index(),
            name: DisplayValue::from_str(window.name()),
            panes: window
                .panes()
                .iter()
                .map(|pane| PaneView::from_snapshot(pane, session_cwd))
                .collect(),
        }
    }

    fn tree(&self, palette: Palette) -> Tree<String> {
        let mut tree = Tree::new(format!(
            "[{}] {}",
            self.source_index,
            palette.bold(self.name.as_str())
        ));
        for pane in &self.panes {
            tree.push(pane.tree(palette));
        }
        tree
    }
}

#[derive(Debug, Eq, PartialEq)]
struct PaneView {
    source_index: u32,
    fact: PaneFact,
    working_directory: PaneWorkingDirectory,
}

impl PaneView {
    fn from_snapshot(pane: &tmux_rescue::PaneSnapshot, session_cwd: &[u8]) -> Self {
        let fact = match pane.recovery() {
            PaneRecovery::Idle => PaneFact::Shell,
            PaneRecovery::Automatic(AutomaticRecovery::Codex { session_id }) => {
                PaneFact::ToolSession {
                    name: "Codex",
                    session_id: session_id.as_uuid().to_string(),
                }
            }
            PaneRecovery::Automatic(AutomaticRecovery::ClaudeCode { session_id }) => {
                PaneFact::ToolSession {
                    name: "Claude Code",
                    session_id: session_id.as_uuid().to_string(),
                }
            }
            PaneRecovery::Automatic(AutomaticRecovery::MdBookServe { command }) => {
                PaneFact::Command(CommandView::from_command(command.command()))
            }
            PaneRecovery::Automatic(AutomaticRecovery::BookshelfServe { command }) => {
                PaneFact::Command(CommandView::from_command(command.command()))
            }
            PaneRecovery::Manual(command) => PaneFact::Command(CommandView::from_command(command)),
            PaneRecovery::Unavailable(failure) => PaneFact::Unavailable {
                reason: DisplayValue::from_str(failure.message()),
            },
        };
        let working_directory = if pane.working_directory().as_bytes() == session_cwd {
            PaneWorkingDirectory::Session
        } else {
            PaneWorkingDirectory::Explicit(DisplayValue::from_bytes(
                pane.working_directory().as_bytes(),
            ))
        };
        Self {
            source_index: pane.source_index(),
            fact,
            working_directory,
        }
    }

    fn tree(&self, palette: Palette) -> Tree<String> {
        let mut root = format!(
            "[{}] {}",
            self.source_index,
            self.fact.render_title(palette)
        );
        for detail in self.fact.details() {
            write!(root, "\n     {detail}").unwrap();
        }
        match &self.working_directory {
            PaneWorkingDirectory::Session => root.push_str("\n     cwd = session"),
            PaneWorkingDirectory::Explicit(path) => {
                write!(root, "\n     cwd {}", path.as_str()).unwrap();
            }
        }
        Tree::new(root).with_multiline(true)
    }
}

#[derive(Debug, Eq, PartialEq)]
enum PaneWorkingDirectory {
    Session,
    Explicit(DisplayValue),
}

#[derive(Debug, Eq, PartialEq)]
enum PaneFact {
    Shell,
    ToolSession {
        name: &'static str,
        session_id: String,
    },
    Command(CommandView),
    Unavailable {
        reason: DisplayValue,
    },
}

impl PaneFact {
    fn render_title(&self, palette: Palette) -> String {
        match self {
            Self::Shell => palette.bold("shell"),
            Self::ToolSession { name, .. } => palette.bold(name),
            Self::Command(command) => palette.bold(&command.command),
            Self::Unavailable { .. } => format!(
                "{} {}",
                palette.yellow("!"),
                palette.bold("program not captured")
            ),
        }
    }

    fn details(&self) -> Vec<String> {
        match self {
            Self::Shell => Vec::new(),
            Self::ToolSession { session_id, .. } => vec![format!("session {session_id}")],
            Self::Command(command) => {
                vec![format!("executable {}", command.executable.as_str())]
            }
            Self::Unavailable { reason } => vec![format!("reason {}", reason.as_str())],
        }
    }

    fn program_identity(&self) -> &str {
        match self {
            Self::Shell => "shell",
            Self::ToolSession { name, .. } => name,
            Self::Command(command) => &command.program_identity,
            Self::Unavailable { .. } => "not captured",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct CommandView {
    command: String,
    executable: DisplayValue,
    program_identity: String,
}

impl CommandView {
    fn from_command(command: &CapturedCommand) -> Self {
        let executable = command.executable().as_os_str();
        let identity = Path::new(executable).file_name().unwrap_or(executable);
        Self {
            command: display_argv(command.argv().iter().map(|argument| argument.as_bytes())),
            executable: DisplayValue::from_os(executable),
            program_identity: DisplayValue::from_os(identity).0,
        }
    }
}

fn count_noun(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}

fn display_bytes(mut bytes: &[u8]) -> String {
    let mut display = String::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(value) => {
                for character in value.chars() {
                    push_display_character(&mut display, character);
                }
                break;
            }
            Err(error) => {
                let valid_length = error.valid_up_to();
                let valid = std::str::from_utf8(&bytes[..valid_length])
                    .expect("Utf8Error::valid_up_to identifies a valid prefix");
                for character in valid.chars() {
                    push_display_character(&mut display, character);
                }
                let invalid_length = error
                    .error_len()
                    .unwrap_or_else(|| bytes.len() - valid_length);
                for byte in &bytes[valid_length..valid_length + invalid_length] {
                    write!(display, "\\x{byte:02x}").expect("writing to a String cannot fail");
                }
                bytes = &bytes[valid_length + invalid_length..];
            }
        }
    }
    display
}

fn push_display_character(display: &mut String, character: char) {
    match character {
        '\\' => display.push_str("\\\\"),
        '"' => display.push_str("\\\""),
        '\n' => display.push_str("\\n"),
        '\r' => display.push_str("\\r"),
        '\t' => display.push_str("\\t"),
        character if character.is_control() && u32::from(character) <= 0x7f => {
            write!(display, "\\x{:02x}", u32::from(character))
                .expect("writing to a String cannot fail");
        }
        character if character.is_control() => {
            write!(display, "\\u{{{:x}}}", u32::from(character))
                .expect("writing to a String cannot fail");
        }
        character if is_unicode_display_control(character) => {
            write!(display, "\\u{{{:x}}}", u32::from(character))
                .expect("writing to a String cannot fail");
        }
        character => display.push(character),
    }
}

fn is_unicode_display_control(character: char) -> bool {
    matches!(
        u32::from(character),
        0x061c | 0x200e..=0x200f | 0x2028..=0x202e | 0x2066..=0x206f
    )
}

fn display_argv<'a>(arguments: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut display = String::new();
    for (position, argument) in arguments.into_iter().enumerate() {
        if position > 0 {
            display.push(' ');
        }
        let encoded = display_bytes(argument);
        if argument_needs_quotes(argument) {
            display.push('"');
            display.push_str(&encoded);
            display.push('"');
        } else {
            display.push_str(&encoded);
        }
    }
    display
}

fn argument_needs_quotes(argument: &[u8]) -> bool {
    if argument.is_empty() {
        return true;
    }
    match std::str::from_utf8(argument) {
        Ok(value) => value.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || is_unicode_display_control(character)
                || matches!(character, '"' | '\\')
        }),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use base64::Engine as _;
    use serde_json::{Value, json};
    use tmux_rescue::{LoadedSnapshot, StateStore};

    use super::*;

    fn encoded(value: &str) -> Value {
        json!({"encoding": "utf8", "value": value})
    }

    fn encoded_bytes(value: &[u8]) -> Value {
        match std::str::from_utf8(value) {
            Ok(value) => encoded(value),
            Err(_) => json!({
                "encoding": "base64",
                "value": base64::engine::general_purpose::STANDARD.encode(value),
            }),
        }
    }

    fn command(executable: &str, arguments: &[&str]) -> Value {
        json!({
            "executable": encoded(executable),
            "argv": arguments.iter().map(|argument| encoded(argument)).collect::<Vec<_>>(),
        })
    }

    fn load_fixture(value: Value) -> (tempfile::TempDir, LoadedSnapshot) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.json");
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let loaded = StateStore::load_explicit_path(&path).unwrap();
        (directory, loaded)
    }

    fn mapped_recovery_fixture() -> Value {
        json!({
            "captured_at": "2026-07-24T05:31:32.581307924+08:00",
            "source": encoded("/tmp/tmux-1000/default"),
            "consistency": {"kind": "stable"},
            "sessions": [{
                "name": "automatic",
                "working_directory": encoded("/workspace"),
                "windows": [{
                    "source_index": 0,
                    "name": "manual",
                    "panes": [
                        {
                            "source_index": 0,
                            "working_directory": encoded("/workspace"),
                            "recovery": {"kind": "idle"},
                        },
                        {
                            "source_index": 1,
                            "working_directory": encoded("/workspace/codex"),
                            "recovery": {
                                "kind": "automatic",
                                "recovery": {
                                    "kind": "codex",
                                    "session_id": "019f7ac5-a55c-7e70-8b31-872ae70c9a94",
                                },
                            },
                        },
                        {
                            "source_index": 2,
                            "working_directory": encoded("/workspace/claude"),
                            "recovery": {
                                "kind": "automatic",
                                "recovery": {
                                    "kind": "claude_code",
                                    "session_id": "8f707f38-6fd3-4a11-a03f-853b03d47b0c",
                                },
                            },
                        },
                        {
                            "source_index": 3,
                            "working_directory": encoded("/workspace/mdbook"),
                            "recovery": {
                                "kind": "automatic",
                                "recovery": {
                                    "kind": "md_book_serve",
                                    "command": command("/usr/bin/mdbook", &["mdbook", "serve"]),
                                },
                            },
                        },
                        {
                            "source_index": 4,
                            "working_directory": encoded("/workspace/book"),
                            "recovery": {
                                "kind": "automatic",
                                "recovery": {
                                    "kind": "bookshelf_serve",
                                    "command": command("/usr/bin/book", &["book", "serve"]),
                                },
                            },
                        },
                        {
                            "source_index": 5,
                            "working_directory": encoded("/workspace/custom"),
                            "recovery": {
                                "kind": "manual",
                                "command": command(
                                    "/usr/local/bin/tmux-rescue",
                                    &["tmux-rescue", "manual", "two words"],
                                ),
                            },
                        },
                        {
                            "source_index": 6,
                            "working_directory": encoded("/workspace/missing"),
                            "recovery": {
                                "kind": "unavailable",
                                "failure": "foreground process disappeared",
                            },
                        },
                    ],
                }],
            }],
        })
    }

    fn topology_fixture() -> Value {
        json!({
            "captured_at": "2026-07-24T05:31:32.581307924+08:00",
            "source": encoded("/tmp/tmux-1000/default"),
            "consistency": {"kind": "stable"},
            "sessions": [
                {
                    "name": "MetaNC",
                    "working_directory": encoded("/home/huwei/projects/MetaNC"),
                    "windows": [
                        {
                            "source_index": 0,
                            "name": "node",
                            "panes": [
                                {
                                    "source_index": 0,
                                    "working_directory": encoded("/home/huwei/projects/MetaNC"),
                                    "recovery": {
                                        "kind": "automatic",
                                        "recovery": {
                                            "kind": "codex",
                                            "session_id": "019f7ac5-a55c-7e70-8b31-872ae70c9a94",
                                        },
                                    },
                                },
                                {
                                    "source_index": 1,
                                    "working_directory": encoded("/home/huwei/projects/MetaNC"),
                                    "recovery": {"kind": "idle"},
                                },
                            ],
                        },
                        {
                            "source_index": 1,
                            "name": "zsh",
                            "panes": [{
                                "source_index": 0,
                                "working_directory": encoded(
                                    "/home/huwei/projects/MetaNC/.worktrees/inspect",
                                ),
                                "recovery": {
                                    "kind": "unavailable",
                                    "failure": "foreground process disappeared",
                                },
                            }],
                        },
                    ],
                },
                {
                    "name": "notes",
                    "working_directory": encoded("/home/huwei/notes"),
                    "windows": [{
                        "source_index": 4,
                        "name": "shell",
                        "panes": [{
                            "source_index": 2,
                            "working_directory": encoded("/home/huwei/notes"),
                            "recovery": {"kind": "idle"},
                        }],
                    }],
                },
            ],
        })
    }

    #[test]
    fn encodes_lossless_values_without_terminal_controls() {
        assert_eq!(display_bytes(b"/tmp/plain"), "/tmp/plain");
        assert_eq!(display_bytes("/tmp/数据".as_bytes()), "/tmp/数据");
        assert_eq!(
            display_bytes("a\u{61c}b\u{200e}c\u{2028}d\u{202e}e\u{2066}f".as_bytes()),
            "a\\u{61c}b\\u{200e}c\\u{2028}d\\u{202e}e\\u{2066}f"
        );
        assert_eq!(
            display_bytes(b"quote\"slash\\tab\tescape\x1b"),
            "quote\\\"slash\\\\tab\\tescape\\x1b"
        );
        assert_eq!(display_bytes(&[b'f', 0x80, b'o']), "f\\x80o");
    }

    #[test]
    fn preserves_argv_boundaries_in_diagnostic_commands() {
        let arguments: &[&[u8]] = &[
            b"cmd",
            b"",
            b"two words",
            b"quote\"",
            b"slash\\",
            &[0x80],
            "数据".as_bytes(),
        ];

        assert_eq!(
            display_argv(arguments.iter().copied()),
            "cmd \"\" \"two words\" \"quote\\\"\" \"slash\\\\\" \"\\x80\" 数据"
        );
        assert_eq!(
            display_argv(["direction\u{202e}".as_bytes()]),
            "\"direction\\u{202e}\""
        );
    }

    #[test]
    fn aggregates_many_distinct_programs_in_first_seen_order() {
        const DISTINCT_PROGRAMS: usize = 4_096;
        const PANES_PER_WINDOW: usize = 1_024;

        let mut windows = (0..DISTINCT_PROGRAMS / PANES_PER_WINDOW)
            .map(|window_index| {
                let panes = (0..PANES_PER_WINDOW)
                    .map(|pane_index| {
                        let index = window_index * PANES_PER_WINDOW + pane_index;
                        let identity = format!("program-{index:04}");
                        PaneView {
                            source_index: pane_index as u32,
                            fact: PaneFact::Command(CommandView {
                                command: identity.clone(),
                                executable: DisplayValue(identity.clone()),
                                program_identity: identity,
                            }),
                            working_directory: PaneWorkingDirectory::Session,
                        }
                    })
                    .collect();
                WindowView {
                    source_index: window_index as u32,
                    name: DisplayValue::from_str(&format!("window-{window_index}")),
                    panes,
                }
            })
            .collect::<Vec<_>>();
        windows.push(WindowView {
            source_index: windows.len() as u32,
            name: DisplayValue::from_str("duplicates"),
            panes: [0, DISTINCT_PROGRAMS - 1]
                .into_iter()
                .map(|index| {
                    let identity = format!("program-{index:04}");
                    PaneView {
                        source_index: index as u32,
                        fact: PaneFact::Command(CommandView {
                            command: identity.clone(),
                            executable: DisplayValue(identity.clone()),
                            program_identity: identity,
                        }),
                        working_directory: PaneWorkingDirectory::Session,
                    }
                })
                .collect(),
        });
        let sessions = [SessionView {
            name: DisplayValue::from_str("stress"),
            working_directory: DisplayValue("/workspace".to_owned()),
            windows,
        }];

        let programs = ProgramEntry::from_sessions(&sessions);

        assert_eq!(programs.len(), DISTINCT_PROGRAMS);
        assert_eq!(
            programs.first(),
            Some(&ProgramEntry {
                identity: "program-0000".to_owned(),
                count: 2,
            })
        );
        assert_eq!(
            programs.last(),
            Some(&ProgramEntry {
                identity: "program-4095".to_owned(),
                count: 2,
            })
        );
    }

    #[test]
    fn renders_recovery_variants_as_user_facts() {
        let (_directory, loaded) = load_fixture(mapped_recovery_fixture());
        let output = render(
            &loaded,
            &crate::cli::SnapshotSelection::Explicit(loaded.path().to_owned()),
            Palette::plain(),
        );

        for expected in [
            "[0] shell",
            "[1] Codex\n",
            "session 019f7ac5-a55c-7e70-8b31-872ae70c9a94",
            "[2] Claude Code\n",
            "session 8f707f38-6fd3-4a11-a03f-853b03d47b0c",
            "[3] mdbook serve\n",
            "executable /usr/bin/mdbook",
            "[4] book serve\n",
            "executable /usr/bin/book",
            "[5] tmux-rescue manual \"two words\"\n",
            "executable /usr/local/bin/tmux-rescue",
            "[6] ! program not captured\n",
            "reason foreground process disappeared",
        ] {
            assert!(
                output.contains(expected),
                "missing {expected:?} in:\n{output}"
            );
        }
        assert!(output.contains("◆ automatic"));
        assert!(output.contains("[0] manual"));
        assert!(!output.contains("Automatic"));
        assert!(!output.contains("Manual"));
    }

    #[test]
    fn renders_complete_plain_snapshot_tree() {
        let (_directory, loaded) = load_fixture(topology_fixture());
        let expected = format!(
            concat!(
                "Snapshot     explicit\n",
                "Captured     2026-07-24T05:31:32.581307924+08:00\n",
                "Source       /tmp/tmux-1000/default\n",
                "Consistency  ● stable topology\n",
                "File         {}\n",
                "\n",
                "Contents     2 sessions · 3 windows · 4 panes\n",
                "Programs     1 Codex · 2 shells · 1 not captured\n",
                "\n",
                "◆ MetaNC · 2 windows · 3 panes\n",
                "  cwd /home/huwei/projects/MetaNC\n",
                "├─ [0] node\n",
                "│  ├─ [0] Codex\n",
                "│  │       session 019f7ac5-a55c-7e70-8b31-872ae70c9a94\n",
                "│  │       cwd = session\n",
                "│  └─ [1] shell\n",
                "│          cwd = session\n",
                "└─ [1] zsh\n",
                "   └─ [0] ! program not captured\n",
                "           reason foreground process disappeared\n",
                "           cwd /home/huwei/projects/MetaNC/.worktrees/inspect\n",
                "\n",
                "◆ notes · 1 window · 1 pane\n",
                "  cwd /home/huwei/notes\n",
                "└─ [4] shell\n",
                "   └─ [2] shell\n",
                "           cwd = session\n",
            ),
            loaded.path().display(),
        );

        assert_eq!(
            render(
                &loaded,
                &crate::cli::SnapshotSelection::Explicit(loaded.path().to_owned()),
                Palette::plain(),
            ),
            expected
        );
    }

    #[test]
    fn unstable_warning_keeps_the_complete_tree() {
        let mut fixture = topology_fixture();
        fixture["consistency"] = json!({"kind": "unstable", "attempts": 3});
        let (_directory, loaded) = load_fixture(fixture);

        let output = render(
            &loaded,
            &crate::cli::SnapshotSelection::Latest,
            Palette::plain(),
        );

        assert!(output.contains("Consistency  ▲ unstable topology after 3 attempts\n"));
        assert!(output.contains("◆ notes · 1 window · 1 pane"));
        assert!(output.ends_with("   └─ [2] shell\n           cwd = session\n"));
    }

    #[test]
    fn forced_color_styles_only_approved_tokens() {
        let (_directory, loaded) = load_fixture(topology_fixture());
        let selection = crate::cli::SnapshotSelection::Explicit(loaded.path().to_owned());
        let plain = render(&loaded, &selection, Palette::plain());
        let colored = render(&loaded, &selection, Palette::colored());

        for expected in [
            "Snapshot     \x1b[1mexplicit\x1b[0m\n",
            "Consistency  \x1b[32m●\x1b[0m stable topology\n",
            "\x1b[36m◆\x1b[0m \x1b[1mMetaNC\x1b[0m · 2 windows · 3 panes\n",
            "├─ [0] \x1b[1mnode\x1b[0m\n",
            "│  ├─ [0] \x1b[1mCodex\x1b[0m\n",
            "│  └─ [1] \x1b[1mshell\x1b[0m\n",
            "\x1b[33m!\x1b[0m \x1b[1mprogram not captured\x1b[0m\n",
        ] {
            assert!(
                colored.contains(expected),
                "missing styled token {expected:?} in:\n{colored}"
            );
        }
        assert!(colored.contains("Programs     1 Codex · 2 shells · 1 not captured\n\n\x1b[36m◆"));
        assert!(colored.contains("reason foreground process disappeared\n"));
        assert!(!colored.contains("\x1b[31m"));
        assert!(!colored.contains("\x1b[36m├"));
        assert!(!colored.contains("\x1b[33mreason"));

        let mut stripped = anstream::StripStream::new(Vec::new());
        stripped.write_all(colored.as_bytes()).unwrap();
        assert_eq!(String::from_utf8(stripped.into_inner()).unwrap(), plain);

        let mut unstable = topology_fixture();
        unstable["consistency"] = json!({"kind": "unstable", "attempts": 3});
        let (_directory, loaded) = load_fixture(unstable);
        let unstable = render(
            &loaded,
            &crate::cli::SnapshotSelection::Latest,
            Palette::colored(),
        );
        assert!(unstable.contains(concat!(
            "Consistency  \x1b[33m▲\x1b[0m ",
            "\x1b[1munstable topology after 3 attempts\x1b[0m\n",
        )));
    }

    #[test]
    fn hostile_os_values_remain_complete_visible_text() {
        let fixture = json!({
            "captured_at": "2026-07-24T00:00:00Z",
            "source": encoded_bytes(b"/tmp/source\x1b[31m.sock"),
            "consistency": {"kind": "stable"},
            "sessions": [{
                "name": "automatic Manual",
                "working_directory": encoded("/workspace"),
                "windows": [{
                    "source_index": 0,
                    "name": "quoted window",
                    "panes": [{
                        "source_index": 0,
                        "working_directory": encoded_bytes(b"/workspace/\x80\x1b[31m"),
                        "recovery": {
                            "kind": "manual",
                            "command": {
                                "executable": encoded_bytes(b"/usr/bin/\x80"),
                                "argv": [
                                    encoded_bytes(&[0x80]),
                                    encoded(""),
                                    encoded("two words"),
                                    encoded("quote\""),
                                    encoded("slash\\"),
                                    encoded_bytes(b"\x1b[31m"),
                                    encoded("数据"),
                                ],
                            },
                        },
                    }],
                }],
            }],
        });
        let (_directory, loaded) = load_fixture(fixture);

        let output = render(
            &loaded,
            &crate::cli::SnapshotSelection::Explicit(loaded.path().to_owned()),
            Palette::plain(),
        );

        assert!(!output.contains('\x1b'));
        assert!(output.contains("Source       /tmp/source\\x1b[31m.sock\n"));
        assert!(output.contains("Programs     1 \\x80\n"));
        assert!(output.contains("◆ automatic Manual · 1 window · 1 pane\n"));
        assert!(output.contains(concat!(
            "[0] \"\\x80\" \"\" \"two words\" \"quote\\\"\" ",
            "\"slash\\\\\" \"\\x1b[31m\" 数据\n",
        )));
        assert!(output.contains("executable /usr/bin/\\x80\n"));
        assert!(output.contains("cwd /workspace/\\x80\\x1b[31m\n"));
    }

    #[test]
    fn hostile_unicode_text_cannot_reorder_or_split_the_tree() {
        let fixture = json!({
            "captured_at": "2026-07-24T00:00:00Z",
            "source": encoded("/tmp/source"),
            "consistency": {"kind": "stable"},
            "sessions": [{
                "name": "work\u{202e}",
                "working_directory": encoded("/workspace"),
                "windows": [{
                    "source_index": 0,
                    "name": "editor\u{2066}",
                    "panes": [{
                        "source_index": 0,
                        "working_directory": encoded("/workspace"),
                        "recovery": {
                            "kind": "unavailable",
                            "failure": "lost\u{2028}then\u{2029}gone",
                        },
                    }],
                }],
            }],
        });
        let (_directory, loaded) = load_fixture(fixture);

        let output = render(
            &loaded,
            &crate::cli::SnapshotSelection::Latest,
            Palette::plain(),
        );

        for character in ['\u{2028}', '\u{2029}', '\u{202e}', '\u{2066}'] {
            assert!(
                !output.contains(character),
                "raw {character:?} in:\n{output}"
            );
        }
        assert!(output.contains("◆ work\\u{202e} · 1 window · 1 pane\n"));
        assert!(output.contains("└─ [0] editor\\u{2066}\n"));
        assert!(output.contains("reason lost\\u{2028}then\\u{2029}gone\n"));
        assert!(output.ends_with("cwd = session\n"));
    }
}
