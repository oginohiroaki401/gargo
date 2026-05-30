use std::path::{Component, Path};

pub fn is_valid_single_name(name: &str) -> bool {
    let path = Path::new(name);
    !name.is_empty()
        && path.components().count() == 1
        && matches!(path.components().next(), Some(Component::Normal(_)))
}

pub fn is_valid_relative_subpath(input: &str) -> bool {
    if input.is_empty() {
        return false;
    }
    let mut has_normal = false;
    for c in Path::new(input).components() {
        match c {
            Component::Normal(_) => has_normal = true,
            _ => return false,
        }
    }
    has_normal
}

pub fn sort_by_name_case_insensitive<T, F>(items: &mut [T], mut name_of: F)
where
    F: FnMut(&T) -> &str,
{
    items.sort_by_key(|item| name_of(item).to_lowercase());
}

#[cfg(test)]
mod tests {
    use super::{is_valid_relative_subpath, is_valid_single_name, sort_by_name_case_insensitive};

    #[test]
    fn single_name_validation_rejects_paths() {
        assert!(is_valid_single_name("file.txt"));
        assert!(!is_valid_single_name(""));
        assert!(!is_valid_single_name("a/b"));
        assert!(!is_valid_single_name("../a"));
    }

    #[test]
    fn relative_subpath_accepts_nested_segments() {
        assert!(is_valid_relative_subpath("file.txt"));
        assert!(is_valid_relative_subpath("dirA/dirB/README.md"));
        assert!(is_valid_relative_subpath("a/b"));
    }

    #[test]
    fn relative_subpath_rejects_bad_components() {
        assert!(!is_valid_relative_subpath(""));
        assert!(!is_valid_relative_subpath("/etc/passwd"));
        assert!(!is_valid_relative_subpath("../foo"));
        assert!(!is_valid_relative_subpath("foo/../bar"));
        assert!(!is_valid_relative_subpath("./foo"));
    }

    #[test]
    fn sort_by_name_ignores_case() {
        let mut values = vec!["bETA".to_string(), "Alpha".to_string(), "delta".to_string()];
        sort_by_name_case_insensitive(&mut values, |entry| entry);
        assert_eq!(
            values,
            vec![
                String::from("Alpha"),
                String::from("bETA"),
                String::from("delta")
            ]
        );
    }
}
