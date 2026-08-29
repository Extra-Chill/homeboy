//! Test support for enum labels that restate their serde wire form.

/// Assert that every listed enum value's label equals its serialized JSON string.
///
/// The caller must depend on `serde_json`; this crate deliberately has no
/// production dependencies.
#[macro_export]
macro_rules! assert_label_matches_serde {
    ($label:ident, [$($value:expr),+ $(,)?]) => {
        $(
            assert_eq!(
                ::serde_json::to_value($value).expect("serialize enum"),
                ::serde_json::Value::String(($value).$label().to_owned()),
                concat!(
                    stringify!($value),
                    ": label drifted from the serde wire form",
                ),
            );
        )+
    };
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    #[derive(Clone, Copy, Serialize)]
    #[serde(rename_all = "snake_case")]
    enum Sample {
        FirstCase,
        SecondCase,
    }

    impl Sample {
        fn as_str(self) -> &'static str {
            match self {
                Self::FirstCase => "first_case",
                Self::SecondCase => "second_case",
            }
        }
    }

    #[test]
    fn pins_matching_labels() {
        crate::assert_label_matches_serde!(as_str, [Sample::FirstCase, Sample::SecondCase]);
    }
}
