use clap::ValueEnum;
use strum::{Display, EnumIter};

#[derive(Debug, Clone, ValueEnum, EnumIter, Display)]
pub enum DatabaseType {
    PostgreSQL,
    MySQL,
    Redis,
    MongoDB,
}

impl DatabaseType {
    pub fn to_slug(&self) -> String {
        match self {
            DatabaseType::PostgreSQL => "postgres".to_string(),
            DatabaseType::MySQL => "mysql".to_string(),
            DatabaseType::Redis => "redis".to_string(),
            DatabaseType::MongoDB => "mongo".to_string(),
        }
    }
}
