use crate::commands::queries::project::PluginType;

impl ToString for PluginType {
    fn to_string(&self) -> String {
        match self {
            PluginType::postgresql => "PostgreSQL".to_owned(),
            PluginType::mysql => "MySQL".to_owned(),
            PluginType::redis => "Redis".to_owned(),
            PluginType::mongodb => "MongoDB".to_owned(),
            PluginType::Other(other) => other.to_owned(),
        }
    }
}
