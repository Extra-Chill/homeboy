pub(super) fn line_of_offset(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_of_offset() {
        assert_eq!(line_of_offset("one\ntwo\nthree", 0), 1);
        assert_eq!(line_of_offset("one\ntwo\nthree", 4), 2);
        assert_eq!(line_of_offset("one\ntwo\nthree", usize::MAX), 3);
    }
}
