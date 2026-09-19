// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use uuid::Uuid;

#[derive(Debug, PartialEq, Clone, Eq, Hash)]
pub struct Guid(pub String);

impl Default for Guid {
    fn default() -> Self {
        Self::new()
    }
}

impl Guid {
    pub fn new() -> Self {
        let guid = Uuid::new_v4().to_string();
        Self(guid)
    }
}

impl std::fmt::Display for Guid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
