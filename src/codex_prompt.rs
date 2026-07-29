use unicode_width::UnicodeWidthStr;

use crate::{
    CapturedCodexPromptArea, CodexPromptCaptureFailure, MAX_CODEX_PROMPT_BYTES, VisiblePaneGrid,
    VisibleRow,
};

const PROMPT_PREFIXES: [&str; 2] = ["› ", "» "];
const TEXTAREA_MARGIN: &str = "  ";

struct PositionedFooterCandidate<'a> {
    content: &'a str,
    row_width: usize,
    pane_width: usize,
}

enum SupportedCodexFooter {
    Instructional(ExactInstructionalFooter),
    Configured(ConfiguredFooterBasis),
}

struct ExactInstructionalFooter;
struct ExactInstructionalHint;

enum ConfiguredFooterBasis {
    High(HighTrustSignal),
    Corroborated(CorroboratedWeakSignals),
}

enum ConfiguredFooterEvidence {
    High(HighTrustSignal),
    Weak(WeakEvidence),
}

enum HighTrustSignal {
    Context,
    LeadingModel,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WeakSignalFamily {
    Model,
    Workspace,
    Accounting,
    Runtime,
    Git,
    Identity,
}

struct CorroboratedWeakSignals {
    _first: WeakSignalFamily,
    _second: WeakSignalFamily,
}

enum WeakEvidence {
    None,
    One(WeakSignalFamily),
    Corroborated(CorroboratedWeakSignals),
}

struct ConfiguredStatusLeftZone<'a>(&'a str);

struct ModelSelection;
struct ContextPercentage;
struct CompactCount;
struct StrictDottedVersion;
struct CanonicalUuid;

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
    let Some(supported_footer) =
        parse_supported_codex_footer(rows, cursor_y, usize::from(metadata.width().get()))
    else {
        return unsupported_layout();
    };
    match supported_footer {
        SupportedCodexFooter::Instructional(_proof) => {}
        SupportedCodexFooter::Configured(ConfiguredFooterBasis::High(_proof)) => {}
        SupportedCodexFooter::Configured(ConfiguredFooterBasis::Corroborated(_proof)) => {}
    }

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
        let Some(_faint_single_row_placeholder) =
            rows[cursor_y].faint_suffix_after_non_faint_prefix(prompt_prefix)
        else {
            return unsupported_layout();
        };
        return CodexPromptAreaObservation::Absent;
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

fn parse_supported_codex_footer(
    rows: &[VisibleRow],
    cursor_y: usize,
    pane_width: usize,
) -> Option<SupportedCodexFooter> {
    let candidate = parse_positioned_footer_candidate(rows, cursor_y, pane_width)?;
    parse_exact_instructional_footer(candidate.content)
        .map(SupportedCodexFooter::Instructional)
        .or_else(|| parse_configured_footer(candidate).map(SupportedCodexFooter::Configured))
}

fn parse_positioned_footer_candidate(
    rows: &[VisibleRow],
    cursor_y: usize,
    pane_width: usize,
) -> Option<PositionedFooterCandidate<'_>> {
    let after_cursor = rows.get(cursor_y + 1..)?;
    let footer_offset = after_cursor
        .iter()
        .position(|row| !row.as_str().is_empty())?;
    if footer_offset == 0 {
        return None;
    }
    let footer_y = cursor_y + 1 + footer_offset;
    if rows[footer_y + 1..]
        .iter()
        .any(|row| !row.as_str().is_empty())
    {
        return None;
    }
    let row = rows[footer_y].as_str();
    let content = strip_textarea_margin(row)?;
    (!content.is_empty() && !content.starts_with(' ')).then_some(PositionedFooterCandidate {
        content,
        row_width: UnicodeWidthStr::width(row),
        pane_width,
    })
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

fn parse_exact_instructional_footer(content: &str) -> Option<ExactInstructionalFooter> {
    parse_exact_instructional_hint(content)
        .map(|_| ExactInstructionalFooter)
        .or_else(|| {
            let without_suffix = content.strip_suffix("% context left")?;
            let percentage_start = without_suffix
                .as_bytes()
                .iter()
                .rposition(|byte| !byte.is_ascii_digit())
                .map_or(0, |index| index + 1);
            let percentage = &without_suffix[percentage_start..];
            if percentage.is_empty()
                || percentage.parse::<u16>().map_or(true, |value| value > 100)
                || (percentage_start > 0
                    && !without_suffix.as_bytes()[percentage_start - 1].is_ascii_whitespace())
            {
                return None;
            }
            let base = without_suffix[..percentage_start]
                .trim_end_matches(|character: char| character.is_ascii_whitespace());
            parse_exact_instructional_hint(base).map(|_| ExactInstructionalFooter)
        })
}

fn parse_exact_instructional_hint(hint: &str) -> Option<ExactInstructionalHint> {
    const BASES: [&str; 4] = [
        "",
        "? for shortcuts",
        "tab to queue",
        "tab to queue message",
    ];
    const PLANS: [&str; 2] = ["Plan mode", "Plan mode (shift+tab to cycle)"];

    (BASES.contains(&hint)
        || PLANS.contains(&hint)
        || BASES.iter().filter(|base| !base.is_empty()).any(|base| {
            hint.strip_prefix(base)
                .and_then(|suffix| suffix.strip_prefix(" · "))
                .is_some_and(|plan| PLANS.contains(&plan))
        }))
    .then_some(ExactInstructionalHint)
}

fn parse_configured_footer(
    candidate: PositionedFooterCandidate<'_>,
) -> Option<ConfiguredFooterBasis> {
    let left_zone = configured_status_left_zone(&candidate);
    let segments = left_zone.0.split(" · ").collect::<Vec<_>>();
    if segments.iter().any(|segment| segment.is_empty())
        || segments[..segments.len().saturating_sub(1)]
            .iter()
            .any(|segment| segment.contains('…'))
    {
        return None;
    }
    let evidence_segments = match segments.last() {
        Some(last) if last.ends_with('…') => {
            if last
                .strip_suffix('…')
                .expect("a terminal ellipsis has a removable final scalar")
                .contains('…')
            {
                return None;
            }
            &segments[..segments.len() - 1]
        }
        Some(last) if last.contains('…') => return None,
        Some(_) => segments.as_slice(),
        None => return None,
    };

    let mut weak = WeakEvidence::None;
    for (position, segment) in evidence_segments.iter().enumerate() {
        if parse_context_segment(segment).is_some() {
            return ConfiguredFooterEvidence::High(HighTrustSignal::Context).finish();
        }
        if position == 0 && parse_model_selection(segment).is_some() {
            return ConfiguredFooterEvidence::High(HighTrustSignal::LeadingModel).finish();
        }
        if let Some(family) = parse_weak_signal_family(segment, position) {
            weak = weak.insert(family);
        }
    }
    ConfiguredFooterEvidence::Weak(weak).finish()
}

fn configured_status_left_zone<'a>(
    candidate: &PositionedFooterCandidate<'a>,
) -> ConfiguredStatusLeftZone<'a> {
    const INDICATORS: [&str; 5] = [
        "Plan mode (shift+tab to cycle) · IDE context",
        "Plan mode (shift+tab to cycle)",
        "Plan mode · IDE context",
        "IDE context",
        "Plan mode",
    ];
    let right_edge = candidate.pane_width.checked_sub(2);
    if right_edge.is_some_and(|edge| candidate.row_width == edge) {
        for indicator in INDICATORS {
            if let Some(before_indicator) = candidate.content.strip_suffix(indicator) {
                let left = before_indicator.trim_end_matches(' ');
                if left.len() < before_indicator.len() && !left.is_empty() {
                    return ConfiguredStatusLeftZone(left);
                }
            }
        }
    }
    ConfiguredStatusLeftZone(candidate.content)
}

impl WeakEvidence {
    fn insert(self, next: WeakSignalFamily) -> Self {
        match self {
            Self::None => Self::One(next),
            Self::One(first) if first == next => Self::One(first),
            Self::One(first) => Self::Corroborated(CorroboratedWeakSignals {
                _first: first,
                _second: next,
            }),
            Self::Corroborated(proof) => Self::Corroborated(proof),
        }
    }

    fn finish(self) -> Option<CorroboratedWeakSignals> {
        match self {
            Self::Corroborated(proof) => Some(proof),
            Self::None | Self::One(_) => None,
        }
    }
}

impl ConfiguredFooterEvidence {
    fn finish(self) -> Option<ConfiguredFooterBasis> {
        match self {
            Self::High(signal) => Some(ConfiguredFooterBasis::High(signal)),
            Self::Weak(weak) => weak.finish().map(ConfiguredFooterBasis::Corroborated),
        }
    }
}

fn parse_context_segment(segment: &str) -> Option<ContextPercentage> {
    let value = segment.strip_prefix("Context ")?;
    parse_context_percentage(value, "% used").or_else(|| parse_context_percentage(value, "% left"))
}

fn parse_context_percentage(value: &str, suffix: &str) -> Option<ContextPercentage> {
    let percentage = value.strip_suffix(suffix)?;
    (!percentage.is_empty()
        && percentage.bytes().all(|byte| byte.is_ascii_digit())
        && percentage.parse::<u16>().is_ok_and(|value| value <= 100))
    .then_some(ContextPercentage)
}

fn parse_model_selection(segment: &str) -> Option<ModelSelection> {
    let mut parts = segment.split(' ');
    let token = parts.next()?;
    let model = token.strip_prefix("gpt-")?;
    if model.is_empty()
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || !model.bytes().any(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    match (parts.next(), parts.next(), parts.next()) {
        (None, None, None) => Some(ModelSelection),
        (
            Some("default" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"),
            None,
            None,
        ) => Some(ModelSelection),
        (
            Some("default" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"),
            Some("fast"),
            None,
        ) => Some(ModelSelection),
        _ => None,
    }
}

fn parse_compact_count(value: &str) -> Option<CompactCount> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(CompactCount);
    }
    let suffix = value.chars().last()?;
    if !matches!(suffix, 'K' | 'M' | 'B' | 'T') {
        return None;
    }
    let number = &value[..value.len() - suffix.len_utf8()];
    if !number.contains('.') {
        return (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
            .then_some(CompactCount);
    }
    let (integer, fractional) = number.split_once('.')?;
    (!integer.is_empty()
        && integer.bytes().all(|byte| byte.is_ascii_digit())
        && (1..=2).contains(&fractional.len())
        && fractional.bytes().all(|byte| byte.is_ascii_digit()))
    .then_some(CompactCount)
}

fn parse_strict_dotted_version(value: &str) -> Option<StrictDottedVersion> {
    let components = value.split('.').collect::<Vec<_>>();
    (components.len() == 3
        && components.iter().all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        }))
    .then_some(StrictDottedVersion)
}

fn parse_canonical_uuid(value: &str) -> Option<CanonicalUuid> {
    let groups = value.split('-').collect::<Vec<_>>();
    ([8, 4, 4, 4, 12]
        .iter()
        .zip(groups.iter())
        .all(|(expected, group)| {
            group.len() == *expected && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && groups.len() == 5)
        .then_some(CanonicalUuid)
}

fn parse_weak_signal_family(segment: &str, position: usize) -> Option<WeakSignalFamily> {
    if position > 0 && parse_model_selection(segment).is_some() {
        return Some(WeakSignalFamily::Model);
    }
    if segment == "~"
        || segment == "/"
        || (segment.starts_with("~/") && segment.len() > 2)
        || (segment.starts_with('/') && segment.len() > 1)
    {
        return Some(WeakSignalFamily::Workspace);
    }
    if [" used", " window", " in", " out"].iter().any(|suffix| {
        segment
            .strip_suffix(suffix)
            .and_then(parse_compact_count)
            .is_some()
    }) {
        return Some(WeakSignalFamily::Accounting);
    }
    if matches!(
        segment,
        "Starting"
            | "Ready"
            | "Working"
            | "Waiting"
            | "Thinking"
            | "Fast on"
            | "Fast off"
            | "raw output"
    ) {
        return Some(WeakSignalFamily::Runtime);
    }
    if segment == "No changes"
        || segment.strip_prefix("PR #").is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        })
        || segment.strip_prefix('+').is_some_and(|counts| {
            counts.split_once(" -").is_some_and(|(added, removed)| {
                !added.is_empty()
                    && added.bytes().all(|byte| byte.is_ascii_digit())
                    && !removed.is_empty()
                    && removed.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
    {
        return Some(WeakSignalFamily::Git);
    }
    (parse_strict_dotted_version(segment).is_some() || parse_canonical_uuid(segment).is_some())
        .then_some(WeakSignalFamily::Identity)
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
    fn keeps_non_faint_single_line_drafts_for_both_prompt_glyphs() {
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
    fn returns_absent_for_any_style_proven_single_row_placeholder() {
        for (glyph, placeholder) in [
            ('›', "Ask Codex to do anything"),
            ('»', "A future renderer-owned placeholder"),
        ] {
            let grid = styled_empty_composer_grid(
                glyph,
                placeholder,
                "  ? for shortcuts    100% context left",
                0,
            );

            assert!(
                matches!(
                    capture_visible_codex_prompt(&grid),
                    CodexPromptAreaObservation::Absent
                ),
                "expected a style-proven empty composer for {placeholder:?}"
            );
        }
    }

    #[test]
    fn accepts_high_or_two_distinct_low_footer_evidence() {
        for footer in [
            "  Context 0% used",
            "  Context 100% used",
            "  Context 0% left",
            "  Context 100% left",
            "  gpt-5.6-sol ultra",
            "  gpt-5.4 xhigh fast",
            "  gpt-5.6-sol ultra · ~/projects/tmux-rescue",
            "  ~/projects/tmux-rescue · gpt-5.6-sol ultra",
            "  Fast on · 258K window",
            "  Working · 2.55M used",
            "  PR #2 · 0.146.0",
            "  No changes · 1d6381bf-01c5-4c4a-b725-8e376e5ad295",
            "  opaque · Fast on · 258K window",
        ] {
            let grid = compact_grid(&["› pending"], 9, footer);

            assert_eq!(captured_text(&grid), "pending", "footer: {footer:?}");
        }
    }

    #[test]
    fn rejects_insufficient_correlated_or_malformed_footer_evidence() {
        for footer in [
            "  main · gpt-5.6-sol ultra",
            "  ~/projects/tmux-rescue",
            "  258K window",
            "  Fast on",
            "  PR #2",
            "  0.146.0",
            "  258K window · 2.55M used",
            "  Fast on · raw output",
            "  main · gpt-5.6-sol ultra · gpt-5.4 high",
            "  Context 101% used",
            "  Context ９% left",
            "  prose mentioning gpt-5.6-sol ultra",
            "  ~/project · gpt-alpha",
            "  ~/project · gpt-5.6-sol super",
            "  Fast on · 2.555M used",
            "  PR #2 · v0.146.0",
            "  PR #2 · 0.146",
            "  PR #2 · 0.146.0-beta",
            "  PR #2 · 0..146",
            "  PR #2 · ０.146.0",
            "  Context 78% used ·  · Fast on",
            "  Context 78% used | Fast on",
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
    fn parses_structured_signal_boundaries() {
        for model in ["gpt-5.6-sol", "gpt-5.6-sol ultra", "gpt-5.4 xhigh fast"] {
            assert!(parse_model_selection(model).is_some(), "model: {model:?}");
        }
        for model in [
            "gpt-",
            "gpt-alpha",
            "gpt-5.6-sol super",
            "gpt-5.6-sol  ultra",
        ] {
            assert!(parse_model_selection(model).is_none(), "model: {model:?}");
        }

        for count in ["0", "258K", "2.5M", "2.55T"] {
            assert!(parse_compact_count(count).is_some(), "count: {count:?}");
        }
        for count in ["-1", ".5M", "2.M", "2.555M", "2.5m"] {
            assert!(parse_compact_count(count).is_none(), "count: {count:?}");
        }

        for version in ["0.146.0", "12.0.300"] {
            assert!(
                parse_strict_dotted_version(version).is_some(),
                "version: {version:?}"
            );
        }
        for version in ["0.146", "0.146.0-beta", "0..146", "０.146.0"] {
            assert!(
                parse_strict_dotted_version(version).is_none(),
                "version: {version:?}"
            );
        }

        for uuid in [
            "1d6381bf-01c5-4c4a-b725-8e376e5ad295",
            "1D6381BF-01C5-4C4A-B725-8E376E5AD295",
        ] {
            assert!(parse_canonical_uuid(uuid).is_some(), "uuid: {uuid:?}");
        }
    }

    #[test]
    fn uses_only_complete_evidence_before_terminal_truncation() {
        let accepted = [
            "  Context 78% used · ~/very/long/path…",
            "  gpt-5.6-sol ultra · ~/very/long/path…",
            "  Fast on · 258K window · unknown…",
        ];
        let rejected = [
            "  Context 48% u…",
            "  Context 48…",
            "  C…",
            "  gpt-5.6…",
            "  Context 78% used…",
            "  Context 78% used · path… · Fast on",
            "  Context 78% used · path…tail",
            "  Context 78% used · path……",
            "  Context 78% used · path…tail…",
        ];

        for footer in accepted {
            assert_eq!(
                captured_text(&compact_grid(&["› pending"], 9, footer)),
                "pending",
                "footer: {footer:?}"
            );
        }
        for footer in rejected {
            assert!(
                matches!(
                    capture_visible_codex_prompt(&compact_grid(&["› pending"], 9, footer)),
                    CodexPromptAreaObservation::Skipped(_)
                ),
                "footer: {footer:?}"
            );
        }
    }

    #[test]
    fn configured_footer_recognition_is_style_independent() {
        for footer in [
            "  Fast on · 258K window",
            "\x1b[2m  Fast on · 258K window\x1b[22m",
            "  \x1b[38;5;1mFast on\x1b[39m \x1b[2m·\x1b[22m \x1b[38;5;2m258K window\x1b[39m",
        ] {
            assert_eq!(
                captured_text(&compact_grid(&["› pending"], 9, footer)),
                "pending",
                "footer: {footer:?}"
            );
        }
    }

    fn footer_with_right_indicator(width: usize, left: &str, indicator: &str) -> String {
        let occupied =
            UnicodeWidthStr::width(format!("{TEXTAREA_MARGIN}{left}{indicator}").as_str());
        let gap = width
            .checked_sub(2 + occupied)
            .filter(|gap| *gap >= 1)
            .unwrap();
        format!("{TEXTAREA_MARGIN}{left}{}{}", " ".repeat(gap), indicator)
    }

    #[test]
    fn accepts_only_exact_right_aligned_indicator_geometry() {
        for indicator in [
            "Plan mode",
            "Plan mode (shift+tab to cycle)",
            "IDE context",
            "Plan mode · IDE context",
            "Plan mode (shift+tab to cycle) · IDE context",
        ] {
            let footer = footer_with_right_indicator(80, "Context 78% used", indicator);
            assert_eq!(
                captured_text(&grid(
                    80,
                    9,
                    0,
                    false,
                    vec!["› pending".to_owned(), String::new(), footer],
                )),
                "pending",
                "indicator: {indicator:?}"
            );
        }

        for footer in [
            footer_with_right_indicator(80, "Context 78% used", "Review mode"),
            footer_with_right_indicator(79, "Context 78% used", "Plan mode"),
            footer_with_right_indicator(81, "Context 78% used", "Plan mode"),
            "   Context 78% used".to_owned(),
        ] {
            assert!(matches!(
                capture_visible_codex_prompt(&grid(
                    80,
                    9,
                    0,
                    false,
                    vec!["› pending".to_owned(), String::new(), footer],
                )),
                CodexPromptAreaObservation::Skipped(_)
            ));
        }
    }

    #[test]
    fn skips_single_row_text_without_complete_style_proof() {
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

        for (rows, cursor_y, cursor_x) in [
            (
                vec![
                    "› first".to_owned(),
                    "  second".to_owned(),
                    String::new(),
                    "  Context 78% used".to_owned(),
                ],
                0,
                7,
            ),
            (
                vec![
                    "› pending".to_owned(),
                    String::new(),
                    "  Context 78% used".to_owned(),
                    "  gpt-5.6-sol ultra · ~/project".to_owned(),
                ],
                0,
                9,
            ),
        ] {
            assert!(matches!(
                capture_visible_codex_prompt(&grid(80, cursor_x, cursor_y, false, rows)),
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
