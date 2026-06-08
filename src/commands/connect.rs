use anyhow::bail;
use is_terminal::IsTerminal;
use reqwest::Client;
use std::{collections::BTreeMap, fmt::Display};
use tokio::process::Command;
use url::Url;
use which::which;

use crate::commands::ssh::{
    PortForward, ensure_ssh_key, get_service_instance_id, spawn_native_ssh_forward,
};
use crate::controllers::{
    database::DatabaseType,
    environment::get_matched_environment,
    project::{find_service_instance, get_environment_instances, get_project},
    variables::get_service_variables,
};
use crate::errors::RailwayError;
use crate::util::prompt::prompt_select;
use crate::{controllers::project::get_service, queries::project::ProjectProjectServicesEdgesNode};

use super::*;

/// Connect to a database's shell (psql for Postgres, mongosh for MongoDB, etc.)
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway connect postgres\n  railway connect redis --environment production\n\nAutomation notes:\n  Non-interactive runs must pass the database service name.\n  The local database client must be installed before connecting."
)]
pub struct Args {
    /// The name of the database to connect to
    service_name: Option<String>,

    /// Environment to pull variables from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Tunnel to the database over SSH instead of a public TCP proxy. Works
    /// without a public domain; auto-enabled when the service has no public
    /// proxy URL.
    #[clap(long)]
    ssh: bool,

    /// Force the public TCP proxy path and never fall back to SSH.
    #[clap(long = "no-ssh", conflicts_with = "ssh")]
    no_ssh: bool,

    /// Local port to bind for the SSH tunnel (defaults to an ephemeral port).
    #[clap(short = 'P', long)]
    port: Option<u16>,
}

impl Display for ProjectProjectServicesEdgesNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    if args.project.is_some() && args.environment.is_none() {
        bail!("--environment is required when using --project");
    }
    let linked_project = if args.project.is_none() {
        Some(configs.get_linked_project().await?)
    } else {
        None
    };
    let project_id = args
        .project
        .clone()
        .or_else(|| linked_project.as_ref().map(|lp| lp.project.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!("No project specified. Use --project or run `railway link` first")
        })?;
    let environment = match args.environment.clone() {
        Some(env) => env,
        None => linked_project
            .as_ref()
            .context("No environment linked. Use --environment when using --project")?
            .environment_id()?
            .to_string(),
    };

    let project = get_project(&client, &configs, project_id.clone()).await?;

    let service = if let Some(name) = args.service_name.clone() {
        get_service(&project, name)?
    } else if std::io::stdout().is_terminal() {
        let nodes_to_prompt = project
            .services
            .edges
            .iter()
            .map(|s| s.node.clone())
            .collect::<Vec<ProjectProjectServicesEdgesNode>>();

        if nodes_to_prompt.is_empty() {
            return Err(RailwayError::ProjectHasNoServices.into());
        }

        prompt_select("Select service", nodes_to_prompt).context("No service selected")?
    } else {
        bail!(
            "Service name required in non-interactive mode. Usage: railway connect <service-name>"
        );
    };

    let environment_id = get_matched_environment(&project, environment)?.id;
    let service_id = service.id.clone();
    let environment_instances =
        get_environment_instances(&client, &configs, &project_id, &environment_id).await?;
    let variables = get_service_variables(
        &client,
        &configs,
        project_id,
        environment_id.clone(),
        service_id.clone(),
    )
    .await?;
    let database_type = {
        let service_instance = find_service_instance(&environment_instances, &service_id);

        service_instance
            .and_then(|si| si.source.clone())
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
        let use_ssh = if args.no_ssh {
            false
        } else if args.ssh {
            true
        } else {
            // Auto: fall back to SSH when there's no public proxy URL to use.
            !has_public_proxy(&db_type, &variables)
        };

        if use_ssh {
            run_ssh_connect(
                &client,
                &configs,
                &db_type,
                &variables,
                &environment_id,
                &service_id,
                args.port,
            )
            .await?;
        } else {
            let (cmd_name, cmd_args) = get_connect_command(&db_type, &variables)?;

            if which(cmd_name.clone()).is_err() {
                bail!("{} must be installed to continue", cmd_name);
            }

            Command::new(cmd_name.as_str())
                .args(cmd_args)
                .spawn()?
                .wait()
                .await?;
        }

        Ok(())
    } else {
        bail!("No supported database found in service")
    }
}

fn get_connect_command(
    database_type: &DatabaseType,
    variables: &BTreeMap<String, String>,
) -> Result<(String, Vec<String>)> {
    match database_type {
        DatabaseType::PostgreSQL => get_postgres_command(variables),
        DatabaseType::Redis => get_redis_command(variables),
        DatabaseType::MongoDB => get_mongo_command(variables),
        DatabaseType::MySQL => get_mysql_command(variables),
    }
}

fn host_is_tcp_proxy(connect_url: String) -> bool {
    connect_url.contains("proxy.rlwy.net")
}

/// Whether the service exposes a public TCP-proxy URL for this engine — the
/// variable `connect`'s public path needs. Drives auto-fallback: when there's
/// no public proxy, `connect` tunnels over SSH instead of erroring.
fn has_public_proxy(database_type: &DatabaseType, variables: &BTreeMap<String, String>) -> bool {
    let (public_key, private_key) = connection_url_keys(database_type);
    variables
        .get(public_key)
        .or_else(|| variables.get(private_key))
        .map(|url| host_is_tcp_proxy(url.clone()))
        .unwrap_or(false)
}

/// The `(public, private)` connection-URL variable names and the engine's
/// in-container default port.
fn connection_url_keys(database_type: &DatabaseType) -> (&'static str, &'static str) {
    match database_type {
        DatabaseType::PostgreSQL => ("DATABASE_PUBLIC_URL", "DATABASE_URL"),
        DatabaseType::Redis => ("REDIS_PUBLIC_URL", "REDIS_URL"),
        DatabaseType::MongoDB => ("MONGO_PUBLIC_URL", "MONGO_URL"),
        DatabaseType::MySQL => ("MYSQL_PUBLIC_URL", "MYSQL_URL"),
    }
}

fn default_remote_port(database_type: &DatabaseType) -> u16 {
    match database_type {
        DatabaseType::PostgreSQL => 5432,
        DatabaseType::Redis => 6379,
        DatabaseType::MongoDB => 27017,
        DatabaseType::MySQL => 3306,
    }
}

/// Connect to the database through an SSH tunnel into the service's container,
/// forwarding a local port to the engine's loopback listener. Needs no public
/// domain or TCP proxy.
#[allow(clippy::too_many_arguments)]
async fn run_ssh_connect(
    client: &Client,
    configs: &Configs,
    database_type: &DatabaseType,
    variables: &BTreeMap<String, String>,
    environment_id: &str,
    service_id: &str,
    requested_port: Option<u16>,
) -> Result<()> {
    let local_port = match requested_port {
        Some(port) => port,
        None => pick_ephemeral_port()?,
    };

    let (cmd_name, cmd_args, remote_port) =
        get_ssh_connect_command(database_type, variables, local_port)?;

    if which(cmd_name.clone()).is_err() {
        bail!("{} must be installed to continue", cmd_name);
    }

    let identity = ensure_ssh_key(client, configs).await?;
    let ssh_target = get_service_instance_id(client, configs, environment_id, service_id).await?;

    eprintln!("Opening SSH tunnel: 127.0.0.1:{local_port} → service :{remote_port} ...");

    // Hold the guard for the lifetime of the client; dropping it kills ssh.
    let _forward = spawn_native_ssh_forward(
        &ssh_target,
        identity.as_deref(),
        &[PortForward {
            local_port,
            remote_port,
        }],
    )?;

    Command::new(cmd_name.as_str())
        .args(cmd_args)
        .spawn()?
        .wait()
        .await?;

    Ok(())
}

/// Reserve an ephemeral local port by binding then immediately releasing it so
/// ssh can claim it for the `-L` forward. (Small TOCTOU window, same as any
/// pick-a-free-port approach.)
fn pick_ephemeral_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("Failed to reserve a local port for the SSH tunnel")?;
    Ok(listener.local_addr()?.port())
}

/// Build the client invocation for the SSH path: rewrite the engine's private
/// connection URL to point at the local tunnel (`127.0.0.1:<local_port>`) while
/// keeping credentials and database name intact. Returns the client binary, its
/// args, and the remote (in-container) port the tunnel must reach.
fn get_ssh_connect_command(
    database_type: &DatabaseType,
    variables: &BTreeMap<String, String>,
    local_port: u16,
) -> Result<(String, Vec<String>, u16)> {
    let (url, remote_port) = local_tunnel_url(database_type, variables, local_port)?;

    let (cmd, args) = match database_type {
        DatabaseType::PostgreSQL => ("psql".to_string(), vec![url.to_string()]),
        DatabaseType::Redis => (
            "redis-cli".to_string(),
            vec!["-u".to_string(), url.to_string()],
        ),
        DatabaseType::MongoDB => ("mongosh".to_string(), vec![url.to_string()]),
        DatabaseType::MySQL => {
            let user = url.username().to_string();
            let password = url.password().unwrap_or("").to_string();
            let database = url.path().trim_start_matches('/').to_string();
            (
                "mysql".to_string(),
                vec![
                    "-h".to_string(),
                    "127.0.0.1".to_string(),
                    "-u".to_string(),
                    user,
                    "-P".to_string(),
                    local_port.to_string(),
                    "-D".to_string(),
                    database,
                    format!("-p{password}"),
                ],
            )
        }
    };

    Ok((cmd, args, remote_port))
}

/// Parse the engine's private connection URL (falling back to the public one
/// only for credentials) and rewrite host/port to the local tunnel endpoint.
/// Returns the rewritten URL plus the in-container port the tunnel must reach.
fn local_tunnel_url(
    database_type: &DatabaseType,
    variables: &BTreeMap<String, String>,
    local_port: u16,
) -> Result<(Url, u16)> {
    let (public_key, private_key) = connection_url_keys(database_type);
    let default_port = default_remote_port(database_type);

    // Prefer the internal URL: it carries the real in-container port. The public
    // proxy URL is only a credentials fallback — its port is the proxy's, not
    // the engine's, so fall back to the default in-container port there.
    let (raw, remote_port_override) = if let Some(internal) = variables.get(private_key) {
        (internal.clone(), None)
    } else if let Some(public) = variables.get(public_key) {
        (public.clone(), Some(default_port))
    } else {
        return Err(RailwayError::ConnectionVariableNotFound(private_key.to_string()).into());
    };

    let mut url = Url::parse(&raw).map_err(|_err| RailwayError::InvalidConnectionVariable)?;
    let remote_port = remote_port_override.unwrap_or_else(|| url.port().unwrap_or(default_port));

    url.set_host(Some("127.0.0.1"))
        .map_err(|_err| RailwayError::InvalidConnectionVariable)?;
    url.set_port(Some(local_port))
        .map_err(|_err| RailwayError::InvalidConnectionVariable)?;

    Ok((url, remote_port))
}

fn get_postgres_command(variables: &BTreeMap<String, String>) -> Result<(String, Vec<String>)> {
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

    Ok(("psql".to_string(), vec![connect_url]))
}

fn get_redis_command(variables: &BTreeMap<String, String>) -> Result<(String, Vec<String>)> {
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

    Ok(("redis-cli".to_string(), vec!["-u".to_string(), connect_url]))
}

fn get_mongo_command(variables: &BTreeMap<String, String>) -> Result<(String, Vec<String>)> {
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

    Ok(("mongosh".to_string(), vec![connect_url]))
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
        "mysql".to_string(),
        vec![
            "-h".to_string(),
            host.to_string(),
            "-u".to_string(),
            user.to_string(),
            "-P".to_string(),
            port.to_string(),
            "-D".to_string(),
            database.to_string(),
            pass_arg,
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
        assert!(!host_is_tcp_proxy("postgres.railway.internal".to_string()));
    }

    #[test]
    fn test_has_public_proxy() {
        let mut variables = BTreeMap::new();
        variables.insert(
            "DATABASE_URL".to_string(),
            "postgresql://postgres:secret@db.railway.internal:5432/railway".to_string(),
        );
        // Only a private URL → no public proxy → auto path uses SSH.
        assert!(!has_public_proxy(&DatabaseType::PostgreSQL, &variables));

        variables.insert(
            "DATABASE_PUBLIC_URL".to_string(),
            "postgresql://postgres:secret@monorail.proxy.rlwy.net:55555/railway".to_string(),
        );
        assert!(has_public_proxy(&DatabaseType::PostgreSQL, &variables));
    }

    #[test]
    fn test_ssh_connect_command_rewrites_to_local_tunnel() {
        let mut variables = BTreeMap::new();
        variables.insert(
            "DATABASE_URL".to_string(),
            "postgresql://postgres:secret@monorail.railway.internal:5432/railway".to_string(),
        );

        let (cmd, args, remote_port) =
            get_ssh_connect_command(&DatabaseType::PostgreSQL, &variables, 49152).unwrap();

        assert_eq!(cmd, "psql");
        assert_eq!(remote_port, 5432);
        assert_eq!(
            args,
            vec!["postgresql://postgres:secret@127.0.0.1:49152/railway".to_string()]
        );
    }

    #[test]
    fn test_ssh_connect_command_falls_back_to_public_url_for_creds() {
        // No private URL — only the public proxy URL is present. We reuse it for
        // credentials but must NOT use its (proxy) port as the remote port.
        let mut variables = BTreeMap::new();
        variables.insert(
            "DATABASE_PUBLIC_URL".to_string(),
            "postgresql://postgres:secret@monorail.proxy.rlwy.net:55555/railway".to_string(),
        );

        let (_cmd, args, remote_port) =
            get_ssh_connect_command(&DatabaseType::PostgreSQL, &variables, 6000).unwrap();

        assert_eq!(remote_port, 5432);
        assert_eq!(
            args,
            vec!["postgresql://postgres:secret@127.0.0.1:6000/railway".to_string()]
        );
    }

    #[test]
    fn test_ssh_connect_command_mysql_args() {
        let mut variables = BTreeMap::new();
        variables.insert(
            "MYSQL_URL".to_string(),
            "mysql://user:password@mysql.railway.internal:3306/railway".to_string(),
        );

        let (cmd, args, remote_port) =
            get_ssh_connect_command(&DatabaseType::MySQL, &variables, 33060).unwrap();

        assert_eq!(cmd, "mysql");
        assert_eq!(remote_port, 3306);
        assert_eq!(
            args,
            vec![
                "-h".to_string(),
                "127.0.0.1".to_string(),
                "-u".to_string(),
                "user".to_string(),
                "-P".to_string(),
                "33060".to_string(),
                "-D".to_string(),
                "railway".to_string(),
                "-ppassword".to_string(),
            ]
        );
    }

    #[test]
    fn test_gets_postgres_command() {
        let private_postgres_url =
            "postgresql://postgres:password@name.railway.internal:5432/railway".to_string();
        let public_postgres_url =
            "postgresql://postgres:password@roundhouse.proxy.rlwy.net:55555/railway".to_string();

        // Valid DATABASE_PUBLIC_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert(
                "DATABASE_PUBLIC_URL".to_string(),
                public_postgres_url.clone(),
            );
            variables.insert("DATABASE_URL".to_string(), private_postgres_url.clone());

            let (cmd, args) = get_postgres_command(&variables).unwrap();
            assert_eq!(cmd, "psql");
            assert_eq!(args, vec![public_postgres_url.clone()]);
        }

        // Valid DATABASE_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("DATABASE_URL".to_string(), public_postgres_url.clone());

            let (cmd, args) = get_postgres_command(&variables).unwrap();
            assert_eq!(cmd, "psql");
            assert_eq!(args, vec![public_postgres_url.clone()]);
        }

        {
            let variables = BTreeMap::new();
            let res = get_postgres_command(&variables);
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

            let res = get_postgres_command(&variables);
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

        // Valid REDIS_PUBLIC_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("REDIS_PUBLIC_URL".to_string(), public_redis_url.clone());
            variables.insert("REDIS_URL".to_string(), private_redis_url.clone());

            let (cmd, args) = get_redis_command(&variables).unwrap();
            assert_eq!(cmd, "redis-cli");
            assert_eq!(args, vec!["-u".to_string(), public_redis_url.clone()]);
        }

        // Valid REDIS_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("REDIS_URL".to_string(), public_redis_url.clone());

            let (cmd, args) = get_redis_command(&variables).unwrap();
            assert_eq!(cmd, "redis-cli");
            assert_eq!(args, vec!["-u".to_string(), public_redis_url.clone()]);
        }

        // No public Redis URL
        {
            let variables = BTreeMap::new();
            let res = get_redis_command(&variables);
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

        // Valid MONGO_PUBLIC_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("MONGO_PUBLIC_URL".to_string(), public_mongo_url.clone());
            variables.insert("MONGO_URL".to_string(), private_mongo_url.clone());

            let (cmd, args) = get_mongo_command(&variables).unwrap();
            assert_eq!(cmd, "mongosh");
            assert_eq!(args, vec![public_mongo_url.clone()]);
        }

        // Valid MONGO_URL
        {
            let mut variables = BTreeMap::new();
            variables.insert("MONGO_URL".to_string(), public_mongo_url.clone());

            let (cmd, args) = get_mongo_command(&variables).unwrap();
            assert_eq!(cmd, "mongosh");
            assert_eq!(args, vec![public_mongo_url.clone()]);
        }

        // No public Mongo URL
        {
            let variables = BTreeMap::new();
            let res = get_mongo_command(&variables);
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

            let res = get_mongo_command(&variables);
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
            assert_eq!(cmd, "mysql");
            assert_eq!(
                args,
                vec![
                    "-h".to_string(),
                    "roundhouse.proxy.rlwy.net".to_string(),
                    "-u".to_string(),
                    "user".to_string(),
                    "-P".to_string(),
                    "12345".to_string(),
                    "-D".to_string(),
                    "railway".to_string(),
                    "-ppassword".to_string(),
                ]
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
