//! Editor and toolchain temp-file patterns that should never touch the
//! remote. Syncing these is always wrong: the file lives for seconds,
//! then the editor deletes it, and the sync layer ends up with half-
//! uploaded files, spurious "object not found" errors, and DB rows
//! that provoke later mirror-delete passes.

pub fn is_editor_temp(name: &str) -> bool {
    if name.ends_with(".kate-swp") {
        return true;
    }
    if name.starts_with('.')
        && (name.ends_with(".swp") || name.ends_with(".swo") || name.ends_with(".swn"))
    {
        return true;
    }
    if name.starts_with(".#") {
        return true;
    }
    if name.starts_with('#') && name.ends_with('#') {
        return true;
    }
    if name.ends_with('~') {
        return true;
    }
    if name.starts_with(".goutputstream-") {
        return true;
    }
    if name.ends_with(".crdownload") || name.ends_with(".part") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_offenders_match() {
        assert!(is_editor_temp(".foo.kate-swp"));
        assert!(is_editor_temp(".bar.swp"));
        assert!(is_editor_temp(".bar.swo"));
        assert!(is_editor_temp(".bar.swn"));
        assert!(is_editor_temp(".#baz"));
        assert!(is_editor_temp("#baz#"));
        assert!(is_editor_temp("baz~"));
        assert!(is_editor_temp(".goutputstream-abc"));
        assert!(is_editor_temp("file.crdownload"));
        assert!(is_editor_temp("file.part"));
    }

    #[test]
    fn real_files_dont_match() {
        assert!(!is_editor_temp("real-file.txt"));
        assert!(!is_editor_temp("swapfile"));
        assert!(!is_editor_temp(".hidden"));
    }
}
