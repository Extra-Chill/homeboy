#[test]
fn artifact_tail_keeps_the_last_lines_in_original_order() {
    assert_eq!(super::tail_lines("one\ntwo\nthree\n", 2), "two\nthree");
    assert_eq!(super::tail_lines("one\ntwo", 0), "");
}
