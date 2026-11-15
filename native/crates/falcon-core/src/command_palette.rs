#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Session,
    Project,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub label: String,
    pub meta: String,
    pub keywords: Vec<String>,
    pub group: Group,
}

pub fn filter(entries: &[Entry], query: &str) -> Vec<usize> {
    let query = query.trim();
    let (group, needle) = match query.chars().next() {
        Some('@') => (Some(Group::Session), query[1..].trim()),
        Some('#') => (Some(Group::Project), query[1..].trim()),
        Some('>') => (Some(Group::Command), query[1..].trim()),
        _ => (None, query),
    };
    let needle = needle.to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if group.is_some_and(|group| group != entry.group) {
                return None;
            }
            let matched = needle.is_empty()
                || std::iter::once(entry.label.as_str())
                    .chain(std::iter::once(entry.meta.as_str()))
                    .chain(entry.keywords.iter().map(String::as_str))
                    .any(|value| value.to_lowercase().contains(&needle));
            matched.then_some(index)
        })
        .collect()
}

pub fn move_selection(selected: usize, count: usize, delta: isize) -> usize {
    if count == 0 {
        0
    } else {
        (selected as isize + delta).rem_euclid(count as isize) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Entry> {
        vec![
            Entry {
                id: "s".into(),
                label: "Claude session".into(),
                meta: "/work/app".into(),
                keywords: vec!["terminal".into()],
                group: Group::Session,
            },
            Entry {
                id: "p".into(),
                label: "Falcon".into(),
                meta: "/work/falcon".into(),
                keywords: vec!["project".into()],
                group: Group::Project,
            },
            Entry {
                id: "c".into(),
                label: "Open Settings".into(),
                meta: String::new(),
                keywords: vec!["preferences".into()],
                group: Group::Command,
            },
        ]
    }

    #[test]
    fn prefixes_limit_groups_and_search_label_meta_keywords_case_insensitively() {
        let rows = rows();
        assert_eq!(filter(&rows, "@ /WORK"), vec![0]);
        assert_eq!(filter(&rows, "# project"), vec![1]);
        assert_eq!(filter(&rows, "> PREF"), vec![2]);
        assert_eq!(filter(&rows, "claude"), vec![0]);
        assert!(filter(&rows, "not found").is_empty());
    }

    #[test]
    fn selection_wraps_and_empty_results_are_safe() {
        assert_eq!(move_selection(0, 3, -1), 2);
        assert_eq!(move_selection(2, 3, 1), 0);
        assert_eq!(move_selection(0, 0, 1), 0);
    }
}
