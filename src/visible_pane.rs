use std::num::NonZeroU16;

use thiserror::Error;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum VisiblePaneGridError {
    #[error("tmux pane id is not valid UTF-8")]
    PaneIdNotUtf8,
    #[error("tmux pane id is not a percent sign followed by ASCII digits")]
    InvalidPaneId,
    #[error("pane width must be nonzero")]
    ZeroWidth,
    #[error("pane height must be nonzero")]
    ZeroHeight,
    #[error("cursor cell is outside the visible pane")]
    CursorOutOfBounds,
    #[error("tmux capture output is not valid UTF-8")]
    OutputNotUtf8,
    #[error("tmux styled capture contains a truncated escape sequence")]
    TruncatedEscapeSequence,
    #[error("tmux styled capture contains a non-SGR escape sequence")]
    NonSgrEscapeSequence,
    #[error("tmux styled capture contains malformed SGR parameters")]
    MalformedSgrParameters,
    #[error("tmux styled capture contains an unsupported SGR operation")]
    UnsupportedSgrOperation,
    #[error("tmux capture output does not end in exactly one output delimiter")]
    MissingFinalDelimiter,
    #[error("tmux capture output has {actual} rows, expected {expected}")]
    RowCount { actual: usize, expected: usize },
    #[error("tmux capture row {row} contains a control character")]
    ControlCharacter { row: usize },
    #[error("tmux capture row {row} is {width} cells wide, exceeding pane width {maximum}")]
    RowTooWide {
        row: usize,
        width: usize,
        maximum: u16,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TmuxPaneId(String);

impl TmuxPaneId {
    pub fn try_from_bytes(value: Vec<u8>) -> Result<Self, VisiblePaneGridError> {
        let value = String::from_utf8(value).map_err(|_| VisiblePaneGridError::PaneIdNotUtf8)?;
        if !value.starts_with('%')
            || value.len() == 1
            || !value.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        {
            return Err(VisiblePaneGridError::InvalidPaneId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneWidth(NonZeroU16);

impl PaneWidth {
    pub fn try_new(value: u16) -> Result<Self, VisiblePaneGridError> {
        NonZeroU16::new(value)
            .map(Self)
            .ok_or(VisiblePaneGridError::ZeroWidth)
    }

    pub fn get(&self) -> u16 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneHeight(NonZeroU16);

impl PaneHeight {
    pub fn try_new(value: u16) -> Result<Self, VisiblePaneGridError> {
        NonZeroU16::new(value)
            .map(Self)
            .ok_or(VisiblePaneGridError::ZeroHeight)
    }

    pub fn get(&self) -> u16 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisibleCellPosition {
    x: u16,
    y: u16,
}

impl VisibleCellPosition {
    pub fn x(&self) -> u16 {
        self.x
    }

    pub fn y(&self) -> u16 {
        self.y
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisiblePaneMetadata {
    pane_id: TmuxPaneId,
    width: PaneWidth,
    height: PaneHeight,
    cursor: VisibleCellPosition,
    in_mode: bool,
}

impl VisiblePaneMetadata {
    pub fn try_new(
        pane_id: TmuxPaneId,
        width: u16,
        height: u16,
        cursor_x: u16,
        cursor_y: u16,
        in_mode: bool,
    ) -> Result<Self, VisiblePaneGridError> {
        let width = PaneWidth::try_new(width)?;
        let height = PaneHeight::try_new(height)?;
        if cursor_x >= width.get() || cursor_y >= height.get() {
            return Err(VisiblePaneGridError::CursorOutOfBounds);
        }
        Ok(Self {
            pane_id,
            width,
            height,
            cursor: VisibleCellPosition {
                x: cursor_x,
                y: cursor_y,
            },
            in_mode,
        })
    }

    pub fn pane_id(&self) -> &TmuxPaneId {
        &self.pane_id
    }

    pub fn width(&self) -> PaneWidth {
        self.width
    }

    pub fn height(&self) -> PaneHeight {
        self.height
    }

    pub fn cursor(&self) -> VisibleCellPosition {
        self.cursor
    }

    pub fn in_mode(&self) -> bool {
        self.in_mode
    }
}

#[derive(Clone)]
pub struct VisibleRow {
    text: String,
    faint_by_char: Vec<bool>,
}

impl std::fmt::Debug for VisibleRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VisibleRow")
            .field("text", &self.text)
            .finish()
    }
}

impl PartialEq for VisibleRow {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for VisibleRow {}

#[allow(dead_code)]
pub(crate) struct FaintVisibleText<'a>(&'a str);

#[allow(dead_code)]
impl FaintVisibleText<'_> {
    pub(crate) fn as_str(&self) -> &str {
        self.0
    }
}

impl VisibleRow {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    #[allow(dead_code)]
    pub(crate) fn faint_suffix_after_non_faint_prefix(
        &self,
        prefix: &str,
    ) -> Option<FaintVisibleText<'_>> {
        let suffix = self.text.strip_prefix(prefix)?;
        let prefix_chars = prefix.chars().count();
        if prefix_chars == 0
            || suffix.is_empty()
            || !self.faint_by_char[..prefix_chars]
                .iter()
                .all(|faint| !faint)
            || !self.faint_by_char[prefix_chars..]
                .iter()
                .all(|faint| *faint)
        {
            return None;
        }
        Some(FaintVisibleText(suffix))
    }
}

#[derive(Clone)]
pub struct VisiblePaneGrid {
    metadata: VisiblePaneMetadata,
    rows: Vec<VisibleRow>,
}

impl std::fmt::Debug for VisiblePaneGrid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VisiblePaneGrid")
            .field("metadata", &self.metadata)
            .field("rows", &self.rows)
            .finish()
    }
}

impl PartialEq for VisiblePaneGrid {
    fn eq(&self, other: &Self) -> bool {
        self.metadata == other.metadata && self.rows == other.rows
    }
}

impl Eq for VisiblePaneGrid {}

impl VisiblePaneGrid {
    pub fn try_from_tmux_styled_capture(
        metadata: VisiblePaneMetadata,
        output: Vec<u8>,
    ) -> Result<Self, VisiblePaneGridError> {
        let (mut output, mut faint_by_char) = decode_styled_capture(&output)?;
        if output.pop() != Some('\n') {
            return Err(VisiblePaneGridError::MissingFinalDelimiter);
        }
        faint_by_char.pop();

        let mut style_start = 0;
        let rows = output
            .split('\n')
            .map(|value| {
                let style_end = style_start + value.chars().count();
                let row = VisibleRow {
                    text: value.to_owned(),
                    faint_by_char: faint_by_char[style_start..style_end].to_vec(),
                };
                style_start = style_end + 1;
                row
            })
            .collect::<Vec<_>>();
        if rows.len() != usize::from(metadata.height().get()) {
            return Err(VisiblePaneGridError::RowCount {
                actual: rows.len(),
                expected: usize::from(metadata.height().get()),
            });
        }
        let maximum = metadata.width().get();
        let rows = rows
            .into_iter()
            .enumerate()
            .map(|(row, value)| {
                if value.text.chars().any(char::is_control) {
                    return Err(VisiblePaneGridError::ControlCharacter { row });
                }
                let width = UnicodeWidthStr::width(value.text.as_str());
                if width > usize::from(maximum) {
                    return Err(VisiblePaneGridError::RowTooWide {
                        row,
                        width,
                        maximum,
                    });
                }
                Ok(value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { metadata, rows })
    }

    pub fn metadata(&self) -> &VisiblePaneMetadata {
        &self.metadata
    }

    pub fn rows(&self) -> &[VisibleRow] {
        &self.rows
    }
}

fn decode_styled_capture(output: &[u8]) -> Result<(String, Vec<bool>), VisiblePaneGridError> {
    let mut text = String::new();
    let mut faint_by_char = Vec::new();
    let mut faint = false;
    let mut ordinary_start = 0;
    let mut index = 0;

    while index < output.len() {
        if output[index] != 0x1b {
            index += 1;
            continue;
        }

        append_ordinary(
            &output[ordinary_start..index],
            faint,
            &mut text,
            &mut faint_by_char,
        )?;
        index += 1;
        if index == output.len() {
            return Err(VisiblePaneGridError::TruncatedEscapeSequence);
        }
        if output[index] != b'[' {
            return Err(VisiblePaneGridError::NonSgrEscapeSequence);
        }
        index += 1;
        let parameters_start = index;
        while index < output.len() && (output[index].is_ascii_digit() || output[index] == b';') {
            index += 1;
        }
        if index == output.len() {
            return Err(VisiblePaneGridError::TruncatedEscapeSequence);
        }
        if output[index] != b'm' {
            return Err(VisiblePaneGridError::NonSgrEscapeSequence);
        }
        apply_sgr(&output[parameters_start..index], &mut faint)?;
        index += 1;
        ordinary_start = index;
    }

    append_ordinary(
        &output[ordinary_start..],
        faint,
        &mut text,
        &mut faint_by_char,
    )?;
    Ok((text, faint_by_char))
}

fn append_ordinary(
    bytes: &[u8],
    faint: bool,
    text: &mut String,
    faint_by_char: &mut Vec<bool>,
) -> Result<(), VisiblePaneGridError> {
    let ordinary = std::str::from_utf8(bytes).map_err(|_| VisiblePaneGridError::OutputNotUtf8)?;
    text.push_str(ordinary);
    faint_by_char.extend(ordinary.chars().map(|_| faint));
    Ok(())
}

fn apply_sgr(parameters: &[u8], faint: &mut bool) -> Result<(), VisiblePaneGridError> {
    if parameters.is_empty() {
        *faint = false;
        return Ok(());
    }
    let parameters = parameters
        .split(|byte| *byte == b';')
        .map(parse_sgr_parameter)
        .collect::<Result<Vec<_>, _>>()?;
    let mut index = 0;

    while index < parameters.len() {
        match parameters[index] {
            0 => *faint = false,
            2 => *faint = true,
            22 => *faint = false,
            38 | 48 | 58 => {
                index = consume_extended_color(&parameters, index)?;
                continue;
            }
            1
            | 3..=9
            | 10..=21
            | 23..=37
            | 39..=47
            | 49..=55
            | 59..=65
            | 73..=75
            | 90..=97
            | 100..=107 => {}
            _ => return Err(VisiblePaneGridError::UnsupportedSgrOperation),
        }
        index += 1;
    }
    Ok(())
}

fn parse_sgr_parameter(value: &[u8]) -> Result<u16, VisiblePaneGridError> {
    if value.is_empty() {
        return Err(VisiblePaneGridError::MalformedSgrParameters);
    }
    value.iter().try_fold(0_u16, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u16::from(byte - b'0')))
            .ok_or(VisiblePaneGridError::MalformedSgrParameters)
    })
}

fn consume_extended_color(parameters: &[u16], index: usize) -> Result<usize, VisiblePaneGridError> {
    let mode = *parameters
        .get(index + 1)
        .ok_or(VisiblePaneGridError::MalformedSgrParameters)?;
    let values = match mode {
        5 => 1,
        2 => 3,
        _ => return Err(VisiblePaneGridError::MalformedSgrParameters),
    };
    let color_start = index + 2;
    let color_end = color_start + values;
    let color = parameters
        .get(color_start..color_end)
        .ok_or(VisiblePaneGridError::MalformedSgrParameters)?;
    if color.iter().any(|value| *value > 255) {
        return Err(VisiblePaneGridError::MalformedSgrParameters);
    }
    Ok(color_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> VisiblePaneMetadata {
        VisiblePaneMetadata::try_new(
            TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
            132,
            40,
            9,
            37,
            false,
        )
        .unwrap()
    }

    fn output(rows: &[&str]) -> Vec<u8> {
        format!("{}\n", rows.join("\n")).into_bytes()
    }

    fn one_row_metadata(width: u16) -> VisiblePaneMetadata {
        VisiblePaneMetadata::try_new(
            TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
            width,
            1,
            0,
            0,
            false,
        )
        .unwrap()
    }

    #[test]
    fn strips_sgr_and_proves_only_a_faint_suffix_after_a_non_faint_prefix() {
        let grid = VisiblePaneGrid::try_from_tmux_styled_capture(
            one_row_metadata(80),
            "\x1b[1m› \x1b[22;2mImplement {feature}\x1b[0m\n"
                .as_bytes()
                .to_vec(),
        )
        .unwrap();
        let row = &grid.rows()[0];

        assert_eq!(row.as_str(), "› Implement {feature}");
        assert_eq!(
            row.faint_suffix_after_non_faint_prefix("› ")
                .unwrap()
                .as_str(),
            "Implement {feature}"
        );
        assert!(row.faint_suffix_after_non_faint_prefix("").is_none());
        assert!(
            row.faint_suffix_after_non_faint_prefix("› Implement")
                .is_none()
        );
    }

    #[test]
    fn conventional_traits_do_not_expose_or_compare_style_evidence() {
        let plain = VisiblePaneGrid::try_from_tmux_styled_capture(
            one_row_metadata(80),
            b"visible\n".to_vec(),
        )
        .unwrap();
        let faint = VisiblePaneGrid::try_from_tmux_styled_capture(
            one_row_metadata(80),
            b"\x1b[2mvisible\x1b[0m\n".to_vec(),
        )
        .unwrap();

        let debug = format!("{faint:?}");
        assert!(!debug.contains("faint"));
        assert_eq!(plain.rows()[0], faint.rows()[0]);
        assert_eq!(plain, faint);
    }

    #[test]
    fn rejects_incomplete_prefix_or_suffix_style_evidence() {
        let cases = [
            "› Implement {feature}\n".as_bytes(),
            "\x1b[2m› Implement {feature}\x1b[0m\n".as_bytes(),
            "›\x1b[2m Implement {feature}\x1b[0m\n".as_bytes(),
            "› \x1b[2mImplement\x1b[22m {feature}\n".as_bytes(),
            "› \n".as_bytes(),
        ];

        for output in cases {
            let grid = VisiblePaneGrid::try_from_tmux_styled_capture(
                one_row_metadata(80),
                output.to_vec(),
            )
            .unwrap();

            assert!(
                grid.rows()[0]
                    .faint_suffix_after_non_faint_prefix("› ")
                    .is_none()
            );
        }
    }

    #[test]
    fn applies_intensity_operations_left_to_right() {
        for (operations, expects_proof) in [("2;22", false), ("22;2", true)] {
            let grid = VisiblePaneGrid::try_from_tmux_styled_capture(
                one_row_metadata(80),
                format!("› \x1b[{operations}mImplement {{feature}}\n").into_bytes(),
            )
            .unwrap();

            assert_eq!(
                grid.rows()[0]
                    .faint_suffix_after_non_faint_prefix("› ")
                    .is_some(),
                expects_proof
            );
        }
    }

    #[test]
    fn carries_faint_state_across_rows() {
        let metadata = VisiblePaneMetadata::try_new(
            TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
            80,
            2,
            0,
            0,
            false,
        )
        .unwrap();
        let grid = VisiblePaneGrid::try_from_tmux_styled_capture(
            metadata,
            "\x1b[2mfirst\n› second\x1b[22m\n".as_bytes().to_vec(),
        )
        .unwrap();

        assert_eq!(grid.rows()[1].as_str(), "› second");
        assert!(grid.rows()[1].faint_by_char.iter().all(|faint| *faint));
    }

    #[test]
    fn accepts_the_finite_sgr_vocabulary_without_treating_color_payload_as_style() {
        for operations in [
            "0",
            "9",
            "10",
            "20",
            "21",
            "29",
            "30",
            "37",
            "39",
            "40",
            "47",
            "49",
            "50",
            "51",
            "55",
            "59",
            "60",
            "65",
            "73",
            "75",
            "90",
            "97",
            "100",
            "107",
            "38;5;2",
            "38;2;2;22;2",
            "48;5;22",
            "48;2;2;22;2",
            "58;5;2",
            "58;2;2;22;2",
        ] {
            assert!(
                VisiblePaneGrid::try_from_tmux_styled_capture(
                    one_row_metadata(80),
                    format!("\x1b[{operations}mplain\n").into_bytes(),
                )
                .is_ok()
            );
        }

        let grid = VisiblePaneGrid::try_from_tmux_styled_capture(
            one_row_metadata(80),
            "\x1b[1m› \x1b[22;2;38;5;22;48;2;2;22;2;58;2;2;22;2mImplement\n"
                .as_bytes()
                .to_vec(),
        )
        .unwrap();

        assert_eq!(
            grid.rows()[0]
                .faint_suffix_after_non_faint_prefix("› ")
                .unwrap()
                .as_str(),
            "Implement"
        );
    }

    #[test]
    fn rejects_unsupported_or_malformed_terminal_sequences_without_echoing_text() {
        let cases: &[&[u8]] = &[
            b"\x1b",
            b"\x1bx\n",
            b"\x1b[2K\n",
            b"\x1b[?2m\n",
            b"\x1b[2:m\n",
            b"\x1b[2;m\n",
            b"\x1b[38m\n",
            b"\x1b[38;5m\n",
            b"\x1b[38;5;256m\n",
            b"\x1b[38;2;0;0m\n",
            b"\x1b[38;2;0;0;256m\n",
            b"\x1b[56m\n",
            b"\x1b[66m\n",
            b"\x1b[76m\n",
            b"\x1b[108m\n",
            b"\x9b2m\n",
            b"\xff\n",
            b"\xe7\x1b[2m\x95\n",
        ];

        for output in cases {
            assert!(
                VisiblePaneGrid::try_from_tmux_styled_capture(
                    one_row_metadata(80),
                    output.to_vec(),
                )
                .is_err()
            );
        }

        let error = VisiblePaneGrid::try_from_tmux_styled_capture(
            one_row_metadata(80),
            b"sensitive text\x1b[?2m\n".to_vec(),
        )
        .unwrap_err();
        let display = error.to_string();
        let debug = format!("{error:?}");

        assert!(!display.contains("sensitive text"));
        assert!(!debug.contains("sensitive text"));
        assert!(!display.contains('\x1b'));
        assert!(!debug.contains('\x1b'));
    }

    #[test]
    fn preserves_plain_grid_controls_width_and_final_empty_row_rules_after_decoding() {
        let metadata = VisiblePaneMetadata::try_new(
            TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
            2,
            2,
            0,
            0,
            false,
        )
        .unwrap();
        let grid = VisiblePaneGrid::try_from_tmux_styled_capture(metadata.clone(), "界\n\n".into())
            .unwrap();

        assert_eq!(grid.rows()[0].as_str(), "界");
        assert_eq!(grid.rows()[1].as_str(), "");
        assert!(
            VisiblePaneGrid::try_from_tmux_styled_capture(metadata.clone(), "界界\n\n".into())
                .is_err()
        );
        assert!(
            VisiblePaneGrid::try_from_tmux_styled_capture(metadata.clone(), "\t\n\n".into())
                .is_err()
        );
        assert!(VisiblePaneGrid::try_from_tmux_styled_capture(metadata, "界\n".into()).is_err());
    }

    #[test]
    fn refines_metadata_and_exactly_40_newline_delimited_rows() {
        let rows = vec!["visible"; 40];
        let grid =
            VisiblePaneGrid::try_from_tmux_styled_capture(metadata(), output(&rows)).unwrap();

        assert_eq!(grid.metadata().pane_id().as_str(), "%15");
        assert_eq!(grid.metadata().width().get(), 132);
        assert_eq!(grid.metadata().height().get(), 40);
        assert_eq!(grid.metadata().cursor().x(), 9);
        assert_eq!(grid.metadata().cursor().y(), 37);
        assert!(!grid.metadata().in_mode());
        assert_eq!(grid.rows().len(), 40);
        assert!(grid.rows().iter().all(|row| row.as_str() == "visible"));
    }

    #[test]
    fn rejects_malformed_pane_ids_and_zero_dimensions() {
        for value in [
            b"15".to_vec(),
            b"%".to_vec(),
            b"%-15".to_vec(),
            b"%a".to_vec(),
        ] {
            assert!(TmuxPaneId::try_from_bytes(value).is_err());
        }
        assert!(PaneWidth::try_new(0).is_err());
        assert!(PaneHeight::try_new(0).is_err());
    }

    #[test]
    fn rejects_cursor_outside_the_visible_grid() {
        let pane_id = TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap();

        assert!(VisiblePaneMetadata::try_new(pane_id.clone(), 132, 40, 132, 37, false).is_err());
        assert!(VisiblePaneMetadata::try_new(pane_id, 132, 40, 9, 40, false).is_err());
    }

    #[test]
    fn rejects_invalid_or_structurally_wrong_capture_rows() {
        let cases = [
            b"visible\xff\n".as_slice(),
            b"visible\r\n".as_slice(),
            b"visible\t\n".as_slice(),
            b"visible".as_slice(),
            b"one\ntwo\n".as_slice(),
        ];
        let one_row = VisiblePaneMetadata::try_new(
            TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
            132,
            1,
            0,
            0,
            false,
        )
        .unwrap();

        for output in cases {
            assert!(
                VisiblePaneGrid::try_from_tmux_styled_capture(one_row.clone(), output.to_vec())
                    .is_err()
            );
        }
    }

    #[test]
    fn measures_rows_in_terminal_cells_instead_of_bytes() {
        let pane_id = TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap();
        let two_cell_pane =
            VisiblePaneMetadata::try_new(pane_id.clone(), 2, 1, 0, 0, false).unwrap();
        let too_narrow_pane = VisiblePaneMetadata::try_new(pane_id, 1, 1, 0, 0, false).unwrap();

        assert!(
            VisiblePaneGrid::try_from_tmux_styled_capture(two_cell_pane, "界\n".into()).is_ok()
        );
        assert!(
            VisiblePaneGrid::try_from_tmux_styled_capture(too_narrow_pane, "界\n".into()).is_err()
        );
    }

    #[test]
    fn rejects_rows_wider_than_the_pane_in_terminal_cells() {
        let metadata = VisiblePaneMetadata::try_new(
            TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
            3,
            1,
            0,
            0,
            false,
        )
        .unwrap();

        assert!(VisiblePaneGrid::try_from_tmux_styled_capture(metadata, "界界\n".into()).is_err());
    }

    #[test]
    fn preserves_a_final_empty_row_from_the_trailing_empty_field() {
        let metadata = VisiblePaneMetadata::try_new(
            TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
            132,
            2,
            0,
            0,
            true,
        )
        .unwrap();
        let grid = VisiblePaneGrid::try_from_tmux_styled_capture(metadata, b"prompt\n\n".to_vec())
            .unwrap();

        assert_eq!(grid.rows()[0].as_str(), "prompt");
        assert_eq!(grid.rows()[1].as_str(), "");
    }
}
