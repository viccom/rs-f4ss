use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: String,
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub mtime: SystemTime,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.dir
    }

    pub fn is_file(&self) -> bool {
        !self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_dir() {
        let entry = Entry {
            path: "/dir".to_string(),
            name: "dir".to_string(),
            dir: true,
            size: 0,
            mtime: SystemTime::UNIX_EPOCH,
        };
        assert!(entry.is_dir());
        assert!(!entry.is_file());
    }

    #[test]
    fn test_entry_file() {
        let entry = Entry {
            path: "/file.txt".to_string(),
            name: "file.txt".to_string(),
            dir: false,
            size: 100,
            mtime: SystemTime::UNIX_EPOCH,
        };
        assert!(!entry.is_dir());
        assert!(entry.is_file());
    }

    #[test]
    fn test_entry_default_mtime() {
        let entry = Entry {
            path: "/file".to_string(),
            name: "file".to_string(),
            dir: false,
            size: 0,
            mtime: SystemTime::UNIX_EPOCH,
        };
        assert_eq!(entry.mtime, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn test_entry_path_name() {
        let entry = Entry {
            path: "/a/b.txt".to_string(),
            name: "b.txt".to_string(),
            dir: false,
            size: 0,
            mtime: SystemTime::UNIX_EPOCH,
        };
        assert_eq!(entry.name, "b.txt");
    }
}
