//! CLI page-range parsing and input/output pattern utilities.

#![allow(dead_code)]

use std::path::PathBuf;

/// Parses a comma-separated list of 1-based page numbers and ranges into
/// zero-based indices in written order. Duplicates are kept. Errors name
/// the offending item and the page count.
pub fn parse_ranges(s: &str, page_count: usize) -> Result<Vec<usize>, String> {
    if s.is_empty() {
        return Ok(Vec::new());
    }

    let mut result = Vec::new();

    for item in s.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err("not a page or range: \"\"".to_string());
        }

        if item.contains('-') {
            let pages = parse_span(item, page_count)?;
            result.extend(pages);
        } else {
            let page = parse_single(item, page_count)?;
            result.push(page);
        }
    }

    Ok(result)
}

fn parse_span(item: &str, page_count: usize) -> Result<Vec<usize>, String> {
    if let Some(dash_pos) = item.find('-') {
        let start_str = item[..dash_pos].trim();
        let end_str = item[dash_pos + 1..].trim();

        let start: usize = start_str
            .parse()
            .map_err(|_| format!("not a page or range: \"{}\"", item))?;
        let end: usize = end_str
            .parse()
            .map_err(|_| format!("not a page or range: \"{}\"", item))?;

        if start == 0 {
            return Err("page 0 does not exist".to_string());
        }
        if end == 0 {
            return Err("page 0 does not exist".to_string());
        }
        if start > page_count {
            return Err(format!(
                "page {start} out of range (document has {page_count} page{})",
                if page_count == 1 { "" } else { "s" }
            ));
        }
        if end > page_count {
            return Err(format!(
                "page {end} out of range (document has {page_count} page{})",
                if page_count == 1 { "" } else { "s" }
            ));
        }
        if start > end {
            return Err(format!(
                "not a page or range: \"{}\" reversed (ranges are low-high)",
                item
            ));
        }

        Ok((start..=end).map(|p| p - 1).collect())
    } else {
        Err(format!("not a page or range: \"{}\"", item))
    }
}

fn parse_single(item: &str, page_count: usize) -> Result<usize, String> {
    let page: usize = item
        .parse()
        .map_err(|_| format!("not a page or range: \"{}\"", item))?;

    if page == 0 {
        return Err("page 0 does not exist".to_string());
    }
    if page > page_count {
        return Err(format!(
            "page {page} out of range (document has {page_count} page{})",
            if page_count == 1 { "" } else { "s" }
        ));
    }

    Ok(page - 1)
}

/// Splits an input specification on the last colon when the tail matches
/// the range pattern (all digits, hyphens, and commas). Returns (path, range_text).
/// A plain path returns None for the range text. Windows drive letters and
/// stray text are not split.
pub fn split_input_spec(spec: &str) -> (PathBuf, Option<String>) {
    if let Some(last_colon_pos) = spec.rfind(':') {
        let tail = &spec[last_colon_pos + 1..];

        if is_range_pattern(tail) {
            let path = &spec[..last_colon_pos];
            return (PathBuf::from(path), Some(tail.to_string()));
        }
    }

    (PathBuf::from(spec), None)
}

fn is_range_pattern(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    if !s.chars().next().unwrap().is_ascii_digit() {
        return false;
    }

    s.chars()
        .all(|c| c.is_ascii_digit() || c == '-' || c == ',')
}

/// Replaces the first `%d` in a pattern with the given 1-based page number.
/// Returns an error if the pattern contains no `%d`.
pub fn pattern_path(pattern: &str, n: usize) -> Result<PathBuf, String> {
    let pos = pattern
        .find("%d")
        .ok_or_else(|| format!("pattern '{}' does not contain %d", pattern))?;

    let result = format!("{}{}{}", &pattern[..pos], n, &pattern[pos + 2..]);
    Ok(PathBuf::from(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plain_list() {
        let result = parse_ranges("2,4,12", 20).unwrap();
        assert_eq!(result, vec![1, 3, 11]);
    }

    #[test]
    fn test_parse_single_range() {
        let result = parse_ranges("4-9", 20).unwrap();
        assert_eq!(result, vec![3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn test_parse_mixed() {
        let result = parse_ranges("2,4-9,12", 20).unwrap();
        assert_eq!(result, vec![1, 3, 4, 5, 6, 7, 8, 11]);
    }

    #[test]
    fn test_parse_duplicates_kept() {
        let result = parse_ranges("2,2,4-5,2", 10).unwrap();
        assert_eq!(result, vec![1, 1, 3, 4, 1]);
    }

    #[test]
    fn test_parse_page_zero_rejected() {
        let err = parse_ranges("0", 10).unwrap_err();
        assert_eq!(err, "page 0 does not exist");
    }

    #[test]
    fn test_parse_page_zero_in_range() {
        let err = parse_ranges("0-5", 10).unwrap_err();
        assert_eq!(err, "page 0 does not exist");
    }

    #[test]
    fn test_parse_reversed_range() {
        let err = parse_ranges("9-2", 20).unwrap_err();
        assert_eq!(
            err,
            "not a page or range: \"9-2\" reversed (ranges are low-high)"
        );
    }

    #[test]
    fn test_parse_malformed_incomplete_trailing_dash() {
        let err = parse_ranges("2-", 20).unwrap_err();
        assert_eq!(err, "not a page or range: \"2-\"");
    }

    #[test]
    fn test_parse_out_of_range() {
        let err = parse_ranges("12", 9).unwrap_err();
        assert_eq!(err, "page 12 out of range (document has 9 pages)");
    }

    #[test]
    fn test_parse_out_of_range_singular() {
        let err = parse_ranges("50", 1).unwrap_err();
        assert_eq!(err, "page 50 out of range (document has 1 page)");
    }

    #[test]
    fn test_parse_range_end_out_of_bounds() {
        let err = parse_ranges("5-15", 10).unwrap_err();
        assert_eq!(err, "page 15 out of range (document has 10 pages)");
    }

    #[test]
    fn test_parse_empty_item() {
        let err = parse_ranges("1,,2", 10).unwrap_err();
        assert_eq!(err, "not a page or range: \"\"");
    }

    #[test]
    fn test_parse_only_comma() {
        let err = parse_ranges(",", 10).unwrap_err();
        assert_eq!(err, "not a page or range: \"\"");
    }

    #[test]
    fn test_parse_empty_string() {
        let result = parse_ranges("", 10).unwrap();
        assert_eq!(result, Vec::<usize>::new());
    }

    #[test]
    fn test_parse_whitespace_padding() {
        let result = parse_ranges(" 2 , 4 - 9 ", 20).unwrap();
        assert_eq!(result, vec![1, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn test_split_plain_path() {
        let (path, range) = split_input_spec("a.pdf");
        assert_eq!(path, PathBuf::from("a.pdf"));
        assert_eq!(range, None);
    }

    #[test]
    fn test_split_path_with_range() {
        let (path, range) = split_input_spec("a.pdf:2-9");
        assert_eq!(path, PathBuf::from("a.pdf"));
        assert_eq!(range, Some("2-9".to_string()));
    }

    #[test]
    fn test_split_path_with_invalid_tail() {
        let (path, range) = split_input_spec("a.pdf:x");
        assert_eq!(path, PathBuf::from("a.pdf:x"));
        assert_eq!(range, None);
    }

    #[test]
    fn test_split_windows_path_with_drive() {
        let (path, range) = split_input_spec("C:\\docs\\a.pdf");
        assert_eq!(path, PathBuf::from("C:\\docs\\a.pdf"));
        assert_eq!(range, None);
    }

    #[test]
    fn test_split_path_with_multiple_items() {
        let (path, range) = split_input_spec("a.pdf:2,4");
        assert_eq!(path, PathBuf::from("a.pdf"));
        assert_eq!(range, Some("2,4".to_string()));
    }

    #[test]
    fn test_split_multiple_colons() {
        let (path, range) = split_input_spec("file:name:2-5");
        assert_eq!(path, PathBuf::from("file:name"));
        assert_eq!(range, Some("2-5".to_string()));
    }

    #[test]
    fn test_parse_malformed_range_from_split() {
        let (path, range) = split_input_spec("a.pdf:2-");
        assert_eq!(path, PathBuf::from("a.pdf"));
        assert_eq!(range, Some("2-".to_string()));

        let err = parse_ranges("2-", 20).unwrap_err();
        assert_eq!(err, "not a page or range: \"2-\"");
    }

    #[test]
    fn test_pattern_simple() {
        let result = pattern_path("part-%d.pdf", 5).unwrap();
        assert_eq!(result, PathBuf::from("part-5.pdf"));
    }

    #[test]
    fn test_pattern_png() {
        let result = pattern_path("output-%d.png", 3).unwrap();
        assert_eq!(result, PathBuf::from("output-3.png"));
    }

    #[test]
    fn test_pattern_no_placeholder() {
        let err = pattern_path("report.pdf", 1).unwrap_err();
        assert!(err.contains("does not contain %d"));
    }

    #[test]
    fn test_pattern_multiple_placeholders() {
        let result = pattern_path("page-%d-out-%d.txt", 7).unwrap();
        assert_eq!(result, PathBuf::from("page-7-out-%d.txt"));
    }

    #[test]
    fn test_pattern_at_start() {
        let result = pattern_path("%d-document.pdf", 42).unwrap();
        assert_eq!(result, PathBuf::from("42-document.pdf"));
    }

    #[test]
    fn test_pattern_at_end() {
        let result = pattern_path("export-%d", 8).unwrap();
        assert_eq!(result, PathBuf::from("export-8"));
    }
}
