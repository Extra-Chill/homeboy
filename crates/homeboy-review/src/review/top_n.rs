use std::collections::BTreeMap;

/// Count items by key, keep the top buckets sorted by count desc then key asc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopN<K> {
    pub items: Vec<(K, usize)>,
    pub remainder: usize,
    pub total: usize,
}

impl<K> TopN<K> {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

/// Group `items` by `key`, sort buckets by count desc then key asc, and cap the list.
pub fn top_n_by<T, K, F>(items: impl IntoIterator<Item = T>, mut key: F, cap: usize) -> TopN<K>
where
    K: Ord,
    F: FnMut(&T) -> K,
{
    let mut counts: BTreeMap<K, usize> = BTreeMap::new();
    let mut total = 0usize;

    for item in items {
        total += 1;
        *counts.entry(key(&item)).or_insert(0) += 1;
    }

    let bucket_count = counts.len();
    let mut ordered: Vec<(K, usize)> = counts.into_iter().collect();
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ordered.truncate(cap);

    TopN {
        items: ordered,
        remainder: bucket_count.saturating_sub(cap),
        total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Buckets sort by count desc then key asc, the cap trims the list, and
    /// the remainder and total still count everything (even at cap zero).
    #[test]
    fn counts_sorts_and_caps_buckets() {
        let label = |label: &&str| label.to_string();
        let buckets = top_n_by(
            ["beta", "alpha", "beta", "gamma", "alpha", "alpha"],
            label,
            2,
        );
        assert_eq!(buckets.total, 6);
        assert_eq!(buckets.remainder, 1);
        assert_eq!(
            buckets.items,
            vec![("alpha".to_string(), 3), ("beta".to_string(), 2)]
        );

        let ties = top_n_by(["zeta", "alpha", "middle"], label, 10);
        assert_eq!(
            ties.items,
            vec![
                ("alpha".to_string(), 1),
                ("middle".to_string(), 1),
                ("zeta".to_string(), 1),
            ]
        );
        assert_eq!(ties.remainder, 0);

        let zero_cap = top_n_by(["a", "b", "b"], label, 0);
        assert_eq!(zero_cap.total, 3);
        assert_eq!(zero_cap.remainder, 2);
        assert!(zero_cap.items.is_empty());
        assert!(!zero_cap.is_empty());
    }
}
