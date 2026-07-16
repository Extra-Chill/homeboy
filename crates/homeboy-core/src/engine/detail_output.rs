//! Helpers for rendering bounded per-item detail output.

use serde::Serialize;

pub const DEFAULT_DETAIL_ITEM_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct DetailOutputMetadata {
    pub items_seen: usize,
    pub items_rendered: usize,
    pub item_limit: usize,
    pub omitted_item_count: usize,
    pub truncated: bool,
}

pub fn bounded_items<T>(items: &[T], item_limit: usize) -> (&[T], DetailOutputMetadata) {
    let items_rendered = items.len().min(item_limit);
    let omitted_item_count = items.len().saturating_sub(items_rendered);
    (
        &items[..items_rendered],
        DetailOutputMetadata {
            items_seen: items.len(),
            items_rendered,
            item_limit,
            omitted_item_count,
            truncated: omitted_item_count > 0,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_items_reports_omitted_metadata() {
        let items = [1, 2, 3, 4];
        let (shown, metadata) = bounded_items(&items, 2);

        assert_eq!(shown, &[1, 2]);
        assert_eq!(metadata.items_seen, 4);
        assert_eq!(metadata.items_rendered, 2);
        assert_eq!(metadata.item_limit, 2);
        assert_eq!(metadata.omitted_item_count, 2);
        assert!(metadata.truncated);
    }

    #[test]
    fn bounded_items_handles_zero_limit() {
        let items = [1, 2, 3];
        let (shown, metadata) = bounded_items(&items, 0);

        assert!(shown.is_empty());
        assert_eq!(metadata.items_seen, 3);
        assert_eq!(metadata.items_rendered, 0);
        assert_eq!(metadata.item_limit, 0);
        assert_eq!(metadata.omitted_item_count, 3);
        assert!(metadata.truncated);
    }
}
