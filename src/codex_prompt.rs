use unicode_width::UnicodeWidthStr;

use crate::{
    CapturedCodexPromptArea, CodexPromptCaptureFailure, MAX_CODEX_PROMPT_BYTES, VisiblePaneGrid,
    VisibleRow,
};

// Codex 0.145.0 rotates these faint empty-composer suggestions.
const SUPPORTED_CODEX_0145_EMPTY_SUGGESTIONS: [&str; 12] = [
    "Ask Codex to do anything",
    "Implement {feature}",
    "Use /skills to list available skills",
    "Write tests for @filename",
    "Explain this codebase",
    "Find and fix a bug in @filename",
    "Run /review on my current changes",
    "Summarize recent commits",
    "Improve documentation in @filename",
    "Check recently modified functions for compatibility",
    "How many files have been modified?",
    "Will this algorithm scale well?",
];
const PROMPT_PREFIXES: [&str; 2] = ["› ", "» "];
const TEXTAREA_MARGIN: &str = "  ";

struct SupportedFooterLayout;

enum ConfiguredContextSegment {
    Complete,
    TerminalTruncation,
}

struct SupportedContextPercentage;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CodexPromptAreaObservation {
    Absent,
    Captured(CapturedCodexPromptArea),
    Skipped(CodexPromptCaptureFailure),
}

pub(crate) fn capture_visible_codex_prompt(grid: &VisiblePaneGrid) -> CodexPromptAreaObservation {
    let metadata = grid.metadata();
    let height = usize::from(metadata.height().get());
    let cursor = metadata.cursor();
    let cursor_y = usize::from(cursor.y());
    if metadata.in_mode() || height < 3 || cursor_y > height - 3 {
        return unsupported_layout();
    }

    let rows = grid.rows();
    let Some(_supported_footer_layout) = parse_supported_footer_layout(rows, cursor_y) else {
        return unsupported_layout();
    };

    let mut start_y = None;
    for row_y in (0..=cursor_y).rev() {
        let row = rows[row_y].as_str();
        if strip_prompt_prefix(row).is_some() {
            start_y = Some(row_y);
            break;
        }
        if !row.is_empty() && strip_textarea_margin(row).is_none() {
            return unsupported_layout();
        }
    }
    let Some(start_y) = start_y else {
        return unsupported_layout();
    };

    let cursor_row = rows[cursor_y].as_str();
    let textarea_start_cell = UnicodeWidthStr::width(TEXTAREA_MARGIN);
    if start_y == cursor_y && usize::from(cursor.x()) == textarea_start_cell {
        let prompt_prefix = PROMPT_PREFIXES
            .iter()
            .find(|prefix| cursor_row.starts_with(**prefix))
            .expect("the candidate start row has a supported prompt prefix");
        let Some(suggestion) = rows[cursor_y].faint_suffix_after_non_faint_prefix(prompt_prefix)
        else {
            return unsupported_layout();
        };
        return if SUPPORTED_CODEX_0145_EMPTY_SUGGESTIONS.contains(&suggestion.as_str()) {
            CodexPromptAreaObservation::Absent
        } else {
            unsupported_layout()
        };
    }
    if cursor_row.is_empty() {
        if usize::from(cursor.x()) != textarea_start_cell {
            return unsupported_layout();
        }
    } else if usize::from(cursor.x()) == textarea_start_cell
        || UnicodeWidthStr::width(cursor_row) != usize::from(cursor.x())
    {
        return unsupported_layout();
    }

    let mut prompt_rows = Vec::with_capacity(cursor_y - start_y + 1);
    for (row_offset, row) in rows[start_y..=cursor_y].iter().enumerate() {
        let row = row.as_str();
        let visible_text = if row_offset == 0 {
            strip_prompt_prefix(row).expect("the candidate start row has a supported prompt prefix")
        } else if row.is_empty() {
            ""
        } else {
            let Some(visible_text) = strip_textarea_margin(row) else {
                return unsupported_layout();
            };
            visible_text
        };
        prompt_rows.push(visible_text);
    }
    let text = prompt_rows.join("\n");
    if text.len() > MAX_CODEX_PROMPT_BYTES {
        return CodexPromptAreaObservation::Skipped(CodexPromptCaptureFailure::size_overflow());
    }
    if text.trim().is_empty()
        || text
            .chars()
            .any(|character| character != '\n' && character.is_control())
    {
        return CodexPromptAreaObservation::Skipped(CodexPromptCaptureFailure::unsafe_text());
    }
    match CapturedCodexPromptArea::try_new(text) {
        Ok(prompt_area) => CodexPromptAreaObservation::Captured(prompt_area),
        Err(_) => CodexPromptAreaObservation::Skipped(CodexPromptCaptureFailure::unsafe_text()),
    }
}

fn unsupported_layout() -> CodexPromptAreaObservation {
    CodexPromptAreaObservation::Skipped(CodexPromptCaptureFailure::unsupported_layout())
}

fn parse_supported_footer_layout(
    rows: &[VisibleRow],
    cursor_y: usize,
) -> Option<SupportedFooterLayout> {
    let after_cursor = rows.get(cursor_y + 1..)?;
    let footer_offset = after_cursor
        .iter()
        .position(|row| !row.as_str().is_empty())?;
    if footer_offset == 0 {
        return None;
    }
    let footer_y = cursor_y + 1 + footer_offset;
    if !is_supported_codex_0145_footer(rows[footer_y].as_str())
        || rows[footer_y + 1..]
            .iter()
            .any(|row| !row.as_str().is_empty())
    {
        return None;
    }
    Some(SupportedFooterLayout)
}

fn strip_prompt_prefix(row: &str) -> Option<&str> {
    PROMPT_PREFIXES.iter().find_map(|prefix| {
        (UnicodeWidthStr::width(*prefix) == 2)
            .then(|| row.strip_prefix(prefix))
            .flatten()
    })
}

fn strip_textarea_margin(row: &str) -> Option<&str> {
    (UnicodeWidthStr::width(TEXTAREA_MARGIN) == 2)
        .then(|| row.strip_prefix(TEXTAREA_MARGIN))
        .flatten()
}

fn is_supported_codex_0145_footer(row: &str) -> bool {
    let Some(after_indent) = strip_textarea_margin(row) else {
        return false;
    };
    let footer = after_indent.trim();
    if footer.is_empty() {
        return false;
    }

    let configured_segments = footer.split(" · ").collect::<Vec<_>>();
    if configured_segments
        .iter()
        .all(|segment| !segment.is_empty())
    {
        let complete_context = configured_segments.iter().any(|segment| {
            matches!(
                parse_configured_context_segment(segment),
                Some(ConfiguredContextSegment::Complete)
            )
        });
        let terminal_truncation = configured_segments.last().is_some_and(|segment| {
            matches!(
                parse_configured_context_segment(segment),
                Some(ConfiguredContextSegment::TerminalTruncation)
            )
        });
        if complete_context || terminal_truncation {
            return true;
        }
    }

    if is_known_footer_hint(footer) {
        return true;
    }

    let Some(without_suffix) = footer.strip_suffix("% context left") else {
        return false;
    };
    let percentage_start = without_suffix
        .as_bytes()
        .iter()
        .rposition(|byte| !byte.is_ascii_digit())
        .map_or(0, |index| index + 1);
    let percentage = &without_suffix[percentage_start..];
    if percentage.is_empty()
        || !percentage.bytes().all(|byte| byte.is_ascii_digit())
        || percentage.parse::<u16>().map_or(true, |value| value > 100)
    {
        return false;
    }
    if percentage_start > 0
        && !without_suffix.as_bytes()[percentage_start - 1].is_ascii_whitespace()
    {
        return false;
    }
    is_known_footer_hint(without_suffix[..percentage_start].trim_end())
}

fn parse_configured_context_segment(segment: &str) -> Option<ConfiguredContextSegment> {
    let value = segment.strip_prefix("Context ")?;
    if parse_context_percentage(value, "% used").is_some() {
        return Some(ConfiguredContextSegment::Complete);
    }
    ["% u…", "…"]
        .into_iter()
        .any(|suffix| parse_context_percentage(value, suffix).is_some())
        .then_some(ConfiguredContextSegment::TerminalTruncation)
}

fn parse_context_percentage(value: &str, suffix: &str) -> Option<SupportedContextPercentage> {
    let percentage = value.strip_suffix(suffix)?;
    (!percentage.is_empty()
        && percentage.bytes().all(|byte| byte.is_ascii_digit())
        && percentage.parse::<u16>().is_ok_and(|value| value <= 100))
    .then_some(SupportedContextPercentage)
}

fn is_known_footer_hint(hint: &str) -> bool {
    const BASES: [&str; 4] = [
        "",
        "? for shortcuts",
        "tab to queue",
        "tab to queue message",
    ];
    const PLANS: [&str; 2] = ["Plan mode", "Plan mode (shift+tab to cycle)"];

    BASES.contains(&hint)
        || PLANS.contains(&hint)
        || BASES.iter().filter(|base| !base.is_empty()).any(|base| {
            hint.strip_prefix(base)
                .and_then(|suffix| suffix.strip_prefix(" · "))
                .is_some_and(|plan| PLANS.contains(&plan))
        })
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::{CodexPromptCaptureFailure, TmuxPaneId, VisiblePaneGrid, VisiblePaneMetadata};

    const CONFIGURED_FOOTER: &str = "  gpt-5.6-sol ultra · ~/projects/tmux-rescue · main · Context 78% used · 258K window · Fast on · Approve for me · 2.55M used · Main…";

    fn grid(
        width: u16,
        cursor_x: u16,
        cursor_y: u16,
        in_mode: bool,
        rows: Vec<String>,
    ) -> VisiblePaneGrid {
        let metadata = VisiblePaneMetadata::try_new(
            TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
            width,
            u16::try_from(rows.len()).unwrap(),
            cursor_x,
            cursor_y,
            in_mode,
        )
        .unwrap();
        VisiblePaneGrid::try_from_tmux_styled_capture(
            metadata,
            format!("{}\n", rows.join("\n")).into_bytes(),
        )
        .unwrap()
    }

    fn compact_grid(prompt_rows: &[&str], cursor_x: u16, footer: &str) -> VisiblePaneGrid {
        let mut rows = prompt_rows
            .iter()
            .map(|row| (*row).to_owned())
            .collect::<Vec<_>>();
        let cursor_y = u16::try_from(rows.len() - 1).unwrap();
        rows.push(String::new());
        rows.push(footer.to_owned());
        let widest = rows
            .iter()
            .map(|row| UnicodeWidthStr::width(row.as_str()))
            .max()
            .unwrap();
        grid(
            u16::try_from(widest + 1).unwrap(),
            cursor_x,
            cursor_y,
            false,
            rows,
        )
    }

    fn styled_empty_composer_grid(
        glyph: char,
        suggestion: &str,
        footer: &str,
        terminal_blank_rows: usize,
    ) -> VisiblePaneGrid {
        let mut rows = vec![
            format!("{glyph} \x1b[2m{suggestion}\x1b[22m"),
            String::new(),
            footer.to_owned(),
        ];
        rows.extend((0..terminal_blank_rows).map(|_| String::new()));
        grid(160, 2, 0, false, rows)
    }

    fn captured_text(grid: &VisiblePaneGrid) -> String {
        match capture_visible_codex_prompt(grid) {
            CodexPromptAreaObservation::Captured(prompt_area) => {
                prompt_area.text().as_str().to_owned()
            }
            observation => panic!("expected captured prompt area, got {observation:?}"),
        }
    }

    fn skipped(grid: &VisiblePaneGrid) -> CodexPromptCaptureFailure {
        match capture_visible_codex_prompt(grid) {
            CodexPromptAreaObservation::Skipped(failure) => failure,
            observation => panic!("expected skipped prompt area, got {observation:?}"),
        }
    }

    #[test]
    fn keeps_the_five_row_real_draft_exact_and_style_agnostic() {
        for first_row in [
            "» The test prompt for recovering.",
            "» \x1b[2mThe test prompt for recovering.\x1b[22m",
        ] {
            let mut rows = vec!["arbitrary transcript row".to_owned(); 40];
            rows[33..].clone_from_slice(&[
                first_row.to_owned(),
                String::new(),
                "  Line 1.".to_owned(),
                String::new(),
                "  Line 2.".to_owned(),
                String::new(),
                CONFIGURED_FOOTER.to_owned(),
            ]);
            let grid = grid(132, 9, 37, false, rows);

            let text = captured_text(&grid);

            assert_eq!(
                text,
                "The test prompt for recovering.\n\nLine 1.\n\nLine 2."
            );
            assert_eq!(text.len(), 49);
        }
    }

    #[test]
    fn accepts_both_codex_prompt_glyphs() {
        for glyph in ['›', '»'] {
            let row = format!("{glyph} pending");
            let cursor_x = u16::try_from(UnicodeWidthStr::width(row.as_str())).unwrap();
            let grid = compact_grid(&[&row], cursor_x, "  tab to queue");

            assert_eq!(captured_text(&grid), "pending");
        }
    }

    #[test]
    fn preserves_indentation_soft_wraps_and_a_trailing_empty_row() {
        let grid = compact_grid(
            &["› first", "    indented", "  soft wrap", ""],
            2,
            "  Plan mode (shift+tab to cycle)",
        );

        assert_eq!(captured_text(&grid), "first\n  indented\nsoft wrap\n");
    }

    #[test]
    fn preserves_literal_pasted_content_placeholders() {
        let row = "» before [Pasted Content 12345 chars] after";
        let cursor_x = u16::try_from(UnicodeWidthStr::width(row)).unwrap();
        let grid = compact_grid(&[row], cursor_x, "  47% context left");

        assert_eq!(
            captured_text(&grid),
            "before [Pasted Content 12345 chars] after"
        );
    }

    #[test]
    fn accepts_a_visible_scrolled_suffix_without_claiming_completeness() {
        let rows = ["› visible suffix", "  still visible"];
        let cursor_x = u16::try_from(UnicodeWidthStr::width(rows[1])).unwrap();
        let grid = compact_grid(
            &rows,
            cursor_x,
            "  tab to queue message    98% context left",
        );

        assert_eq!(captured_text(&grid), "visible suffix\nstill visible");
    }

    #[test]
    fn returns_absent_for_each_style_proven_codex_0145_suggestion() {
        let suggestions = [
            ('›', "Ask Codex to do anything"),
            ('»', "Implement {feature}"),
            ('›', "Use /skills to list available skills"),
            ('»', "Write tests for @filename"),
            ('›', "Explain this codebase"),
            ('»', "Find and fix a bug in @filename"),
            ('›', "Run /review on my current changes"),
            ('»', "Summarize recent commits"),
            ('›', "Improve documentation in @filename"),
            ('»', "Check recently modified functions for compatibility"),
            ('›', "How many files have been modified?"),
            ('»', "Will this algorithm scale well?"),
        ];

        for (glyph, suggestion) in suggestions {
            let grid = styled_empty_composer_grid(
                glyph,
                suggestion,
                "  ? for shortcuts    100% context left",
                0,
            );

            assert!(
                matches!(
                    capture_visible_codex_prompt(&grid),
                    CodexPromptAreaObservation::Absent
                ),
                "expected a style-proven empty composer for {suggestion:?}"
            );
        }
    }

    #[test]
    fn accepts_only_observed_terminal_context_truncations() {
        for footer in [
            "  gpt-5.6-sol ultra · ~/project · main · Context 0% u…",
            "  gpt-5.6-sol ultra · ~/project · main · Context 48% u…",
            "  gpt-5.6-sol ultra · ~/project · main · Context 100% u…",
            "  gpt-5.6-sol ultra · ~/project · main · Context 0…",
            "  gpt-5.6-sol ultra · ~/project · main · Context 5…",
            "  gpt-5.6-sol ultra · ~/project · main · Context 100…",
        ] {
            let grid = compact_grid(&["› pending"], 9, footer);

            assert_eq!(captured_text(&grid), "pending", "footer: {footer:?}");
        }

        for footer in [
            "  gpt-5.6-sol ultra · ~/project · main · C…",
            "  gpt-5.6-sol ultra · ~/project · main · Context …",
            "  gpt-5.6-sol ultra · ~/project · main · Context 101% u…",
            "  gpt-5.6-sol ultra · ~/project · main · Context 101…",
            "  gpt-5.6-sol ultra · ~/project · main · Context 5% us…",
            "  gpt-5.6-sol ultra · ~/project · main · Context 5...",
            "  gpt-5.6-sol ultra · ~/project · main · Context ５…",
            "  gpt-5.6-sol ultra · ~/project · main · Context 5% u… · Fast on",
            "  gpt-5.6-sol ultra · ~/project · main · Context 5… · Fast on",
        ] {
            let grid = compact_grid(&["› pending"], 9, footer);

            assert!(
                matches!(
                    capture_visible_codex_prompt(&grid),
                    CodexPromptAreaObservation::Skipped(_)
                ),
                "expected an unsupported footer for {footer:?}"
            );
        }
    }

    #[test]
    fn skips_known_suggestions_without_complete_style_proof() {
        let collisions = [
            "› Ask Codex to do anything".to_owned(),
            "\x1b[2m› Ask Codex to do anything\x1b[22m".to_owned(),
            "› \x1b[2mAsk Codex\x1b[22m to do anything".to_owned(),
            "› \x1b[2mAsk Codex to do \x1b[22manything".to_owned(),
        ];

        for row in collisions {
            let grid = grid(
                80,
                2,
                0,
                false,
                vec![
                    row,
                    String::new(),
                    "  ? for shortcuts    100% context left".to_owned(),
                ],
            );

            assert!(matches!(
                capture_visible_codex_prompt(&grid),
                CodexPromptAreaObservation::Skipped(_)
            ));
        }
    }

    #[test]
    fn skips_an_unknown_faint_suggestion() {
        let grid = styled_empty_composer_grid(
            '›',
            "Ask something else",
            "  ? for shortcuts    100% context left",
            0,
        );

        assert!(matches!(
            capture_visible_codex_prompt(&grid),
            CodexPromptAreaObservation::Skipped(_)
        ));
    }

    #[test]
    fn accepts_a_recognized_footer_followed_by_blank_terminal_rows() {
        for terminal_blank_rows in [0, 5] {
            let real_draft = grid(
                80,
                7,
                0,
                false,
                [
                    "» draft".to_owned(),
                    String::new(),
                    "  95% context left".to_owned(),
                ]
                .into_iter()
                .chain((0..terminal_blank_rows).map(|_| String::new()))
                .collect(),
            );
            assert_eq!(captured_text(&real_draft), "draft");

            let empty_composer = styled_empty_composer_grid(
                '›',
                "Ask Codex to do anything",
                "  95% context left",
                terminal_blank_rows,
            );
            assert!(matches!(
                capture_visible_codex_prompt(&empty_composer),
                CodexPromptAreaObservation::Absent
            ));
        }
    }

    #[test]
    fn rejects_nonblank_or_duplicate_rows_around_the_footer() {
        let prompt = "› \x1b[2mAsk Codex to do anything\x1b[22m";
        let footer = "  95% context left";
        let layouts = [
            vec![prompt.to_owned(), footer.to_owned()],
            vec![prompt.to_owned(), String::new(), String::new()],
            vec![
                prompt.to_owned(),
                String::new(),
                "  shortcut overlay".to_owned(),
                footer.to_owned(),
            ],
            vec![
                prompt.to_owned(),
                String::new(),
                footer.to_owned(),
                footer.to_owned(),
            ],
            vec![
                prompt.to_owned(),
                String::new(),
                footer.to_owned(),
                "  shortcut overlay".to_owned(),
            ],
        ];

        for rows in layouts {
            let grid = grid(80, 2, 0, false, rows);
            assert!(matches!(
                capture_visible_codex_prompt(&grid),
                CodexPromptAreaObservation::Skipped(_)
            ));
        }
    }

    #[test]
    fn accepts_normal_variable_unused_textarea_rows() {
        let footers = [
            "  tab to queue",
            "  tab to queue · Plan mode    0% context left",
            "  Plan mode (shift+tab to cycle)    100% context left",
            "  Context 0% used",
        ];
        for (unused_rows, footer) in (1..=4).zip(footers) {
            let mut rows = vec!["» queued".to_owned()];
            rows.extend((0..unused_rows).map(|_| String::new()));
            rows.push(footer.to_owned());
            let grid = grid(80, 8, 0, false, rows);

            assert_eq!(captured_text(&grid), "queued");
        }
    }

    #[test]
    fn skips_shell_mode_popup_and_unrecognized_bottom_layouts() {
        let shell_mode = compact_grid(&["! shell command"], 15, "  tab to queue");
        assert!(matches!(
            capture_visible_codex_prompt(&shell_mode),
            CodexPromptAreaObservation::Skipped(_)
        ));

        let mut popup_rows = vec![
            "› pending".to_owned(),
            "  shortcut overlay".to_owned(),
            "  tab to queue".to_owned(),
        ];
        let popup = grid(40, 9, 0, false, popup_rows.clone());
        assert!(matches!(
            capture_visible_codex_prompt(&popup),
            CodexPromptAreaObservation::Skipped(_)
        ));
        popup_rows[1] = "╭─ popup ─╮".to_owned();
        let popup = grid(40, 9, 0, false, popup_rows);
        assert!(matches!(
            capture_visible_codex_prompt(&popup),
            CodexPromptAreaObservation::Skipped(_)
        ));

        for footer in [
            "  status",
            "  unknown hint    98% context left",
            "  tab to queue    101% context left",
            "  tab to queue    9x% context left",
            "  tab to queue    ９% context left",
            "  Context 101% used",
            "  Context x% used",
        ] {
            let grid = compact_grid(&["› pending"], 9, footer);
            assert!(matches!(
                capture_visible_codex_prompt(&grid),
                CodexPromptAreaObservation::Skipped(_),
            ));
        }

        let in_mode = grid(
            40,
            9,
            0,
            true,
            vec![
                "› pending".to_owned(),
                String::new(),
                "  tab to queue".to_owned(),
            ],
        );
        assert!(matches!(
            capture_visible_codex_prompt(&in_mode),
            CodexPromptAreaObservation::Skipped(_)
        ));
    }

    #[test]
    fn skips_a_cursor_in_the_middle_of_input() {
        let grid = compact_grid(&["› pending input"], 5, "  tab to queue");

        assert!(matches!(
            capture_visible_codex_prompt(&grid),
            CodexPromptAreaObservation::Skipped(_)
        ));
    }

    #[test]
    fn skips_a_continuation_without_the_two_cell_margin() {
        let row = " continuation";
        let cursor_x = u16::try_from(UnicodeWidthStr::width(row)).unwrap();
        let grid = compact_grid(&["› pending", row], cursor_x, "  tab to queue");

        assert!(matches!(
            capture_visible_codex_prompt(&grid),
            CodexPromptAreaObservation::Skipped(_)
        ));
    }

    #[test]
    fn skips_when_trimmed_trailing_spaces_break_cursor_alignment() {
        let grid = compact_grid(&["› pending"], 11, "  tab to queue");

        assert!(matches!(
            capture_visible_codex_prompt(&grid),
            CodexPromptAreaObservation::Skipped(_)
        ));
    }

    #[test]
    fn skips_unsafe_and_oversized_prompt_text() {
        let unsafe_row = "›  ";
        let unsafe_cursor = u16::try_from(UnicodeWidthStr::width(unsafe_row)).unwrap();
        let unsafe_grid = compact_grid(&[unsafe_row], unsafe_cursor, "  tab to queue");
        let failure = skipped(&unsafe_grid);
        assert!(failure.message().len() <= 4_096);
        assert!(!failure.message().contains(unsafe_row));

        let sensitive = "sensitive-visible-prompt".repeat(750);
        let oversized_row = format!("› {sensitive}");
        let oversized_cursor =
            u16::try_from(UnicodeWidthStr::width(oversized_row.as_str())).unwrap();
        let oversized_grid = compact_grid(
            &[&oversized_row],
            oversized_cursor,
            "  tab to queue message    98% context left",
        );
        let failure = skipped(&oversized_grid);
        assert!(failure.message().len() <= 4_096);
        assert!(!failure.message().contains(&sensitive));
        assert!(!format!("{failure:?}").contains(&sensitive));

        let read_failure = CodexPromptCaptureFailure::visible_pane_read_failed();
        assert_eq!(
            read_failure.message(),
            "visible tmux pane could not be read"
        );
        assert!(!format!("{read_failure:?}").contains(&sensitive));
    }
}
