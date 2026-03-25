//! constants — extracted from contract_testgen.rs.

use std::collections::HashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::extension::grammar::{ContractGrammar, TypeConstructor, TypeDefault};


    pub const EMPTY: &str = "empty";

    pub const NON_EMPTY: &str = "non_empty";

    pub const NONE: &str = "none";

    pub const SOME_DEFAULT: &str = "some_default";

    pub const NONEXISTENT_PATH: &str = "nonexistent_path";

    pub const EXISTENT_PATH: &str = "existent_path";

    pub const TRUE: &str = "true";

    pub const FALSE: &str = "false";

    pub const ZERO: &str = "zero";

    pub const POSITIVE: &str = "positive";

    pub const CONTAINS: &str = "contains";
