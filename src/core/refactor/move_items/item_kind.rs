//! item_kind — extracted from move_items.rs.


#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Function,
    Struct,
    Enum,
    Const,
    Static,
    TypeAlias,
    Impl,
    Trait,
    Test,
    Unknown,
}
