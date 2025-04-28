use clap::ValueEnum;
use strum::{Display, EnumIter};

#[derive(Debug, Clone, EnumIter, Display)]
pub enum DatabaseType {
    PostgreSQL,
    MySQL,
    Redis,
    MongoDB,
}

impl DatabaseType {
    pub fn to_slug(&self) -> &'static str {
        match self {
            DatabaseType::PostgreSQL => "postgres",
            DatabaseType::MySQL => "mysql",
            DatabaseType::Redis => "redis",
            DatabaseType::MongoDB => "mongo",
        }
    }
}

impl ValueEnum for DatabaseType {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::PostgreSQL, Self::MySQL, Self::Redis, Self::MongoDB]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.to_slug()))
    }
}
