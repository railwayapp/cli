use anyhow::bail;
use std::{collections::BTreeMap, fmt::Display};
use std::fs::File;
use std::process::Stdio;
use tokio::process::Command;
use url::Url;
use which::which;

use crate::controllers::{
    database::DatabaseType, environment::get_matched_environment, project::get_project,
    variables::get_service_variables,
};
use crate::errors::RailwayError;
use crate::util::prompt::prompt_select;
use crate::{controllers::project::get_service, queries::project::ProjectProjectServicesEdgesNode};

use super::*;

/// Backs up a database into a file
#[derive(Parser)]
pub struct Args {
    /// The name of the database to connect to
    service_name: Option<String>,

    /// Environment to pull variables from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// File to write the backup to
    #[clap(long = "file", short = 'f', value_name = "FILE", default_value = "backup.sql")]
    filter: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    // Heavily inspired by the `connect` command
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let filename = args
        .filter
        .clone()
        .unwrap_or("backup.sql".to_string());

    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let service = args
        .service_name
        .clone()
        .map(|name| get_service(&project, name))
        .unwrap_or_else(|| {
            let nodes_to_prompt = project
                .services
                .edges
                .iter()
                .map(|s| s.node.clone())
                .collect::<Vec<ProjectProjectServicesEdgesNode>>();

            if nodes_to_prompt.is_empty() {
                return Err(RailwayError::ProjectHasNoServices.into());
            }

            prompt_select("Select service", nodes_to_prompt).context("No service selected")
        })?;

    let environment_id = get_matched_environment(&project, environment)?.id;
    let variables = get_service_variables(
        &client,
        &configs,
        linked_project.project,
        environment_id.clone(),
        service.id,
    )
        .await?;
    let database_type = {
        let service_instance = service
            .service_instances
            .edges
            .iter()
            .find(|si| si.node.environment_id == environment_id);

        service_instance
            .and_then(|si| si.node.source.clone())
            .and_then(|source| source.image)
            .map(|image: String| image.to_lowercase())
            .and_then(|image: String| {
                if image.contains("postgres")
                    || image.contains("postgis")
                    || image.contains("timescale")
                {
                    Some(DatabaseType::PostgreSQL)
                } else if image.contains("redis") {
                    Some(DatabaseType::Redis)
                } else if image.contains("mongo") {
                    Some(DatabaseType::MongoDB)
                } else if image.contains("mysql") {
                    Some(DatabaseType::MySQL)
                } else {
                    None
                }
            })
    };
    if let Some(db_type) = database_type {
        let (cmd_name, args) = get_backup_command(db_type, variables, filename)?;

        if which(cmd_name.clone()).is_err() {
            bail!("{} must be installed to continue", cmd_name);
        }

        let mut cmd = Command::new(cmd_name.as_str());
        cmd.args(args);

        if (cmd_name.as_str() == "mysqldump") {
            // Redirects mysqldump output to a file
            let output = File::create("backup.mysql.sql")?;
            cmd.stdout(Stdio::from(output));
        }

        cmd.spawn()?.wait().await?;

        Ok(())
    } else {
        bail!("No supported database found in service")
    }
}

fn get_backup_command(
    database_type: DatabaseType,
    variables: BTreeMap<String, String>,
    filename: String,
) -> Result<(String, Vec<String>)> {
    match &database_type {
        DatabaseType::PostgreSQL => get_postgres_command(&variables, filename),
        DatabaseType::Redis => get_redis_command(&variables, filename),
        DatabaseType::MongoDB => get_mongo_command(&variables, filename),
        DatabaseType::MySQL => get_mysql_command(&variables),
    }
}

fn host_is_tcp_proxy(connect_url: String) -> bool {
    connect_url.contains("proxy.rlwy.net")
}

fn get_postgres_command(variables: &BTreeMap<String, String>, filename: String) -> Result<(String, Vec<String>)> {
    let connect_url = variables
        .get("DATABASE_PUBLIC_URL")
        .or_else(|| variables.get("DATABASE_URL"))
        .map(|s| s.to_string())
        .ok_or(RailwayError::ConnectionVariableNotFound(
            "DATABASE_PUBLIC_URL".to_string(),
        ))?;

    if !host_is_tcp_proxy(connect_url.clone()) {
        return Err(RailwayError::InvalidConnectionVariable.into());
    }

    Ok((
        "pg_dump".to_string(),
        vec![
            connect_url.to_string(),
            "-F".to_string(), "c".to_string(),
            "-f".to_string(), filename.clone(),
        ],
    ))
}

fn get_redis_command(variables: &BTreeMap<String, String>, filename: String) -> Result<(String, Vec<String>)> {
    let connect_url = variables
        .get("REDIS_PUBLIC_URL")
        .or_else(|| variables.get("REDIS_URL"))
        .map(|s| s.to_string())
        .ok_or(RailwayError::ConnectionVariableNotFound(
            "REDIS_PUBLIC_URL".to_string(),
        ))?;

    if !host_is_tcp_proxy(connect_url.clone()) {
        return Err(RailwayError::InvalidConnectionVariable.into());
    }

    Ok((
        "redis-cli".to_string(),
        vec![
            "-u".to_string(), connect_url.to_string(),
            "--rdb".to_string(), filename.clone(),
        ],
    ))

}

fn get_mongo_command(variables: &BTreeMap<String, String>, filename: String) -> Result<(String, Vec<String>)> {
    let connect_url = variables
        .get("MONGO_PUBLIC_URL")
        .or_else(|| variables.get("MONGO_URL"))
        .map(|s| s.to_string())
        .ok_or(RailwayError::ConnectionVariableNotFound(
            "MONGO_PUBLIC_URL".to_string(),
        ))?;

    if !host_is_tcp_proxy(connect_url.clone()) {
        return Err(RailwayError::InvalidConnectionVariable.into());
    }

    Ok((
        "mongodump".to_string(),
        vec![
            "--uri".to_string(), connect_url.trim().to_string(),
            format!("--archive={}", filename.clone()),
            "--gzip".to_string()
        ],
    ))

}

fn get_mysql_command(variables: &BTreeMap<String, String>) -> Result<(String, Vec<String>)> {
    let connect_url = variables
        .get("MYSQL_PUBLIC_URL")
        .or_else(|| variables.get("MYSQL_URL"))
        .map(|s| s.to_string())
        .ok_or(RailwayError::ConnectionVariableNotFound(
            "MYSQL_PUBLIC_URL".to_string(),
        ))?;

    if !host_is_tcp_proxy(connect_url.clone()) {
        return Err(RailwayError::InvalidConnectionVariable.into());
    }

    let parsed_url =
        Url::parse(&connect_url).map_err(|_err| RailwayError::InvalidConnectionVariable)?;

    let host = parsed_url.host_str().unwrap_or("");
    let user = parsed_url.username();
    let password = parsed_url.password().unwrap_or("");
    let port = parsed_url.port().unwrap_or(3306);
    let database = parsed_url.path().trim_start_matches('/');

    let pass_arg = format!("-p{password}");

    Ok((
        "mysqldump".to_string(),
        vec![
            "-h".to_string(),
            host.to_string(),
            "-u".to_string(),
            user.to_string(),
            "-P".to_string(),
            port.to_string(),
            pass_arg,
            database.to_string()
        ],
    ))
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_is_tcp_proxy() {
        assert!(host_is_tcp_proxy("roundhouse.proxy.rlwy.net".to_string()));
        assert!(!host_is_tcp_proxy("localhost".to_string()));
        assert!(!host_is_tcp_proxy("postgres.railway.interal".to_string()));
    }

    #[test]
    fn test_gets_postgres_command() {
        let private_postgres_url =
            "postgresql://postgres:password@name.railway.internal:5432/railway".to_string();
        let public_postgres_url =
            "postgresql://postgres:password@roundhouse.proxy.rlwy.net:55555/railway".to_string();
        let filename = "backup.sql".to_string();

        // Valid DATABASE_PUBLIC_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert(
                "DATABASE_PUBLIC_URL".to_string(),
                public_postgres_url.clone(),
            );
            variables.insert("DATABASE_URL".to_string(), private_postgres_url.clone());

            let (cmd, args) = get_postgres_command(&variables, filename.clone()).unwrap();
            assert_eq!(cmd, "pg_dump");
            assert_eq!(args, vec![
                public_postgres_url.clone(),
                "-F".to_string(), "c".to_string(),
                "-f".to_string(), "backup.sql".to_string(),
            ]);
        }

        // Valid DATABASE_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("DATABASE_URL".to_string(), public_postgres_url.clone());

            let (cmd, args) = get_postgres_command(&variables, filename.clone()).unwrap();
            assert_eq!(cmd, "pg_dump");
            assert_eq!(args, vec![
                public_postgres_url.clone(),
                "-F".to_string(), "c".to_string(),
                "-f".to_string(), "backup.sql".to_string(),
            ]);
        }

        {
            let variables = BTreeMap::new();
            let res = get_postgres_command(&variables, filename.clone());
            assert!(res.is_err());
            assert_eq!(
                res.unwrap_err().to_string(),
                RailwayError::ConnectionVariableNotFound("DATABASE_PUBLIC_URL".to_string())
                    .to_string()
            );
        }

        // Invalid DATABASE_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("DATABASE_URL".to_string(), private_postgres_url.clone());

            let res = get_postgres_command(&variables, filename.clone());
            assert!(res.is_err());
            assert_eq!(
                res.unwrap_err().to_string(),
                RailwayError::InvalidConnectionVariable.to_string()
            );
        }
    }

    #[test]
    fn test_gets_redis_command() {
        let private_redis_url = "redis://default:password@redis.railway.internal:6379".to_string();
        let public_redis_url = "redis://default:password@monorail.proxy.rlwy.net:26137".to_string();
        let filename = "backup.sql".to_string();

        // Valid REDIS_PUBLIC_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("REDIS_PUBLIC_URL".to_string(), public_redis_url.clone());
            variables.insert("REDIS_URL".to_string(), private_redis_url.clone());

            let (cmd, args) = get_redis_command(&variables, filename.clone()).unwrap();
            assert_eq!(cmd, "redis-cli");
            assert_eq!(args, vec![
                "-u".to_string(), public_redis_url.clone(),
                "--rdb".to_string(), "backup.sql".to_string(),
            ]);
        }

        // Valid REDIS_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("REDIS_URL".to_string(), public_redis_url.clone());

            let (cmd, args) = get_redis_command(&variables, filename.clone()).unwrap();
            assert_eq!(cmd, "redis-cli");
            assert_eq!(args, vec![
                "-u".to_string(), public_redis_url.clone(),
                "--rdb".to_string(), "backup.sql".to_string(),
            ]);
        }

        // No public Redis URL
        {
            let variables = BTreeMap::new();
            let res = get_redis_command(&variables, filename.clone());
            assert!(res.is_err());
            assert_eq!(
                res.unwrap_err().to_string(),
                RailwayError::ConnectionVariableNotFound("REDIS_PUBLIC_URL".to_string())
                    .to_string()
            );
        }
    }

    #[test]
    fn test_gets_mongo_command() {
        let private_mongo_url =
            "mongodb://user:password@mongo.railway.internal:27017/railway".to_string();
        let public_mongo_url =
            "mongodb://user:password@roundhouse.proxy.rlwy.net:33333/railway".to_string();
        let filename = "backup.sql".to_string();

        // Valid MONGO_PUBLIC_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("MONGO_PUBLIC_URL".to_string(), public_mongo_url.clone());
            variables.insert("MONGO_URL".to_string(), private_mongo_url.clone());

            let (cmd, args) = get_mongo_command(&variables, filename.clone()).unwrap();
            assert_eq!(cmd, "mongodump");
            assert_eq!(args, vec![
                "--uri".to_string(), public_mongo_url.clone(),
                "--archive=backup.sql".to_string(),
                "--gzip".to_string()
            ]);
        }

        // Valid MONGO_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("MONGO_URL".to_string(), public_mongo_url.clone());

            let (cmd, args) = get_mongo_command(&variables, filename.clone()).unwrap();
            assert_eq!(cmd, "mongodump");
            assert_eq!(args, vec![
                "--uri".to_string(), public_mongo_url.clone(),
                "--archive=backup.sql".to_string(),
                "--gzip".to_string()
            ]);
        }

        // No public Mongo URL
        {
            let variables = BTreeMap::new();
            let res = get_mongo_command(&variables, filename.clone());
            assert!(res.is_err());
            assert_eq!(
                res.unwrap_err().to_string(),
                RailwayError::ConnectionVariableNotFound("MONGO_PUBLIC_URL".to_string())
                    .to_string()
            );
        }

        // Invalid MONGO_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("MONGO_URL".to_string(), private_mongo_url.clone());

            let res = get_mongo_command(&variables, filename.clone());
            assert!(res.is_err());
            assert_eq!(
                res.unwrap_err().to_string(),
                RailwayError::InvalidConnectionVariable.to_string()
            );
        }
    }

    #[test]
    fn test_gets_mysql_command() {
        let private_mysql_url =
            "mysql://user:password@mysql.railway.internal:3306/railway".to_string();
        let public_mysql_url =
            "mysql://user:password@roundhouse.proxy.rlwy.net:12345/railway".to_string();

        // Valid MYSQL_PUBLIC_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("MYSQL_PUBLIC_URL".to_string(), public_mysql_url.clone());
            variables.insert("MYSQL_URL".to_string(), private_mysql_url.clone());

            let (cmd, args) = get_mysql_command(&variables).unwrap();
            assert_eq!(cmd, "mysqldump");
            assert_eq!(
                args,
                vec![
                    "-h".to_string(),
                    "roundhouse.proxy.rlwy.net".to_string(),
                    "-u".to_string(),
                    "user".to_string(),
                    "-P".to_string(),
                    "12345".to_string(),
                    "-ppassword".to_string(),
                    "railway".to_string(),
                ],
            );
        }

        // Invalid URL format
        {
            let mut variables = BTreeMap::new();
            variables.insert("MYSQL_URL".to_string(), "invalid_url".to_string());

            let res = get_mysql_command(&variables);
            assert!(res.is_err());
            assert_eq!(
                res.unwrap_err().to_string(),
                RailwayError::InvalidConnectionVariable.to_string()
            );
        }
    }
}
