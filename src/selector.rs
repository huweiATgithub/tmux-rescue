use std::ffi::{OsStr, OsString};
use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TmuxSelector {
    SocketName(OsString),
    SocketPath(OsString),
}

impl TmuxSelector {
    pub fn flag(&self) -> &'static str {
        match self {
            Self::SocketName(_) => "-L",
            Self::SocketPath(_) => "-S",
        }
    }

    pub fn value(&self) -> &OsStr {
        match self {
            Self::SocketName(value) | Self::SocketPath(value) => value,
        }
    }

    pub(crate) fn append_to(&self, command: &mut Command) {
        command.arg(self.flag()).arg(self.value());
    }
}

#[cfg(test)]
mod tests {
    use super::TmuxSelector;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::process::Command;

    #[test]
    fn socket_name_appends_the_name_flag_and_preserves_its_value() {
        let selector = TmuxSelector::SocketName(OsString::from("work"));

        let arguments = selector_arguments(&selector);

        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0].as_bytes(), b"-L");
        assert_eq!(arguments[1].as_bytes(), b"work");
    }

    #[test]
    fn socket_path_appends_the_path_flag_and_preserves_its_value() {
        let selector = TmuxSelector::SocketPath(OsString::from("./work.sock"));

        let arguments = selector_arguments(&selector);

        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0].as_bytes(), b"-S");
        assert_eq!(arguments[1].as_bytes(), b"./work.sock");
    }

    #[test]
    fn socket_name_preserves_an_empty_value() {
        let selector = TmuxSelector::SocketName(OsString::new());

        let arguments = selector_arguments(&selector);

        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0].as_bytes(), b"-L");
        assert_eq!(arguments[1].as_bytes(), b"");
    }

    #[test]
    fn socket_path_preserves_non_utf8_bytes() {
        let selector = TmuxSelector::SocketPath(OsString::from_vec(vec![b'.', b'/', 0xff]));

        let arguments = selector_arguments(&selector);

        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0].as_bytes(), b"-S");
        assert_eq!(arguments[1].as_bytes(), &[b'.', b'/', 0xff]);
    }

    fn selector_arguments(selector: &TmuxSelector) -> Vec<OsString> {
        let mut command = Command::new("tmux");
        selector.append_to(&mut command);
        command.get_args().map(OsStr::to_owned).collect()
    }
}
