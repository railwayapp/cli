use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpResponse {
    pub url: String,
    pub logs_url: String,
    pub deployment_domain: String,
}
