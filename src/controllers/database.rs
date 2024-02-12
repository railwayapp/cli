use clap::ValueEnum;
use strum::{Display, EnumIter};

#[derive(Debug, Clone, ValueEnum, EnumIter, Display)]
pub enum DatabaseType {
    PostgreSQL,
    MySQL,
    Redis,
    MongoDB,
}
