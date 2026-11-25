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
        for (limit, shown_len, omitted, truncated) in [
            (2, 2, 2, true),
            (0, 0, 4, true),
            (4, 4, 0, false),
            (10, 4, 0, false),
        ] {
            let (shown, metadata) = bounded_items(&items, limit);
            assert_eq!(shown, &items[..shown_len], "limit {limit}");
            assert_eq!(
                metadata,
                DetailOutputMetadata {
                    items_seen: 4,
                    items_rendered: shown_len,
                    item_limit: limit,
                    omitted_item_count: omitted,
                    truncated,
                },
                "limit {limit}"
            );
        }
    }
}
