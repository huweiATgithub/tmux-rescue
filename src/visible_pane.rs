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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisibleRow(String);

impl VisibleRow {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisiblePaneGrid {
    metadata: VisiblePaneMetadata,
    rows: Vec<VisibleRow>,
}

impl VisiblePaneGrid {
    pub fn try_from_tmux_capture(
        metadata: VisiblePaneMetadata,
        output: Vec<u8>,
    ) -> Result<Self, VisiblePaneGridError> {
        let output = String::from_utf8(output).map_err(|_| VisiblePaneGridError::OutputNotUtf8)?;
        let rows = output
            .strip_suffix('\n')
            .ok_or(VisiblePaneGridError::MissingFinalDelimiter)?
            .split('\n')
            .map(ToOwned::to_owned)
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
                if value.chars().any(char::is_control) {
                    return Err(VisiblePaneGridError::ControlCharacter { row });
                }
                let width = UnicodeWidthStr::width(value.as_str());
                if width > usize::from(maximum) {
                    return Err(VisiblePaneGridError::RowTooWide {
                        row,
                        width,
                        maximum,
                    });
                }
                Ok(VisibleRow(value))
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

    #[test]
    fn refines_metadata_and_exactly_40_newline_delimited_rows() {
        let rows = vec!["visible"; 40];
        let grid = VisiblePaneGrid::try_from_tmux_capture(metadata(), output(&rows)).unwrap();

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
                VisiblePaneGrid::try_from_tmux_capture(one_row.clone(), output.to_vec()).is_err()
            );
        }
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

        assert!(VisiblePaneGrid::try_from_tmux_capture(metadata, "界界\n".into()).is_err());
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
        let grid =
            VisiblePaneGrid::try_from_tmux_capture(metadata, b"prompt\n\n".to_vec()).unwrap();

        assert_eq!(grid.rows()[0].as_str(), "prompt");
        assert_eq!(grid.rows()[1].as_str(), "");
    }
}
