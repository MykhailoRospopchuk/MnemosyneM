use rusqlite::{
    ToSql,
    types::{FromSql, ToSqlOutput},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MediaType {
    #[default]
    Photo = 0,
    Animation = 1,
}

impl MediaType {
    pub fn as_i32(&self) -> i32 {
        *self as i32
    }

    pub fn from_i32(val: i32) -> Self {
        match val {
            1 => MediaType::Animation,
            _ => MediaType::Photo,
        }
    }
}

impl ToSql for MediaType {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_i32()))
    }
}

impl FromSql for MediaType {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let val = i32::column_result(value)?;
        Ok(Self::from_i32(val))
    }
}
