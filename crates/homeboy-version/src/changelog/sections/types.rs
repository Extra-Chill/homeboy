//! types — extracted from sections.rs.

#[derive(Debug, PartialEq)]
pub(crate) enum SectionContentStatus {
    Valid,           // Has bullet items (direct or under subsections)
    SubsectionsOnly, // Has ### headers but no bullets
    Empty,           // Nothing meaningful
}
