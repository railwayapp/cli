use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use colored::Colorize;
use fs2::FileExt;
use tokio::sync::mpsc;

use crate::controllers::config::ServiceInstance;

use super::code_runner::{COLORS, LogLine, ProcessManager, print_log_line};
use super::compose::PortType;
use super::https_proxy::HttpsConfig;
use super::local_config::LocalDevConfig;
use super::output::{ServiceSummary, print_code_service_summary};
use super::ports::{generate_port, get_develop_dir, slugify};
use super::tui;
use super::variables::{
    LocalDevelopContext, inject_mkcert_ca_vars, override_railway_vars, print_domain_info,
};

pub struct DevelopSessionLock {
    _file: File,
    path: PathBuf,
}

impl DevelopSessionLock {
    /// Try to acquire exclusive lock for code services in this project.
    /// Returns Ok(lock) if acquired, Err if another session is running.
    pub fn try_acquire(project_id: &str) -> Result<Self> {
        let develop_dir = get_develop_dir(project_id);
        Self::try_acquire_at(&develop_dir)
    }

    /// Try to acquire lock at a specific directory (for testing)
    pub fn try_acquire_at(develop_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(develop_dir)?;

        let path = develop_dir.join("session.lock");
        let file = File::create(&path)?;

        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(e) if e.kind() == fs2::lock_contended_error().kind() => {
                bail!(
                    "Another develop session is already running for this project.\n\
                     Stop it with Ctrl+C before starting a new one."
                )
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for DevelopSessionLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub struct DevSession {
    process_manager: ProcessManager,
    tui_services: Vec<tui::ServiceInfo>,
    log_rx: mpsc::Receiver<LogLine>,
    docker_rx: mpsc::Receiver<LogLine>,
    output_path: PathBuf,
    has_image_services: bool,
    code_count: usize,
    image_count: usize,
    _session_lock: DevelopSessionLock,
}

#[allow(clippy::too_many_arguments)]
impl DevSession {
    pub async fn start(
        project_id: &str,
        configured_code_services: &[&(&String, &ServiceInstance)],
        service_names: &HashMap<String, String>,
        local_dev_config: &LocalDevConfig,
        code_resolved_vars: &HashMap<String, BTreeMap<String, String>>,
        ctx: &LocalDevelopContext,
        https_config: &Option<HttpsConfig>,
        service_summaries: &[ServiceSummary],
        output_path: PathBuf,
        has_image_services: bool,
        use_tui: bool,
        verbose: bool,
    ) -> Result<Self> {
        let session_lock = DevelopSessionLock::try_acquire(project_id)?;

        println!("{}", "Starting code services...".cyan());

        let (log_tx, log_rx) = mpsc::channel(100);
        let mut process_manager = ProcessManager::new();
        let mut tui_services: Vec<tui::ServiceInfo> = Vec::new();

        for (i, (service_id, svc)) in configured_code_services.iter().enumerate() {
            let dev_config = match local_dev_config.get_service(service_id) {
                Some(c) => c,
                None => continue,
            };

            let service_name = service_names
                .get(*service_id)
                .cloned()
                .unwrap_or_else(|| (*service_id).clone());

            let working_dir = PathBuf::from(&dev_config.directory);

            let internal_port = dev_config
                .port
                .map(|p| p as i64)
                .or_else(|| svc.get_ports().first().copied());
            let proxy_port = internal_port.map(|p| generate_port(service_id, p));

            let raw_vars = code_resolved_vars
                .get(*service_id)
                .cloned()
                .unwrap_or_default();

            let service_domains = ctx
                .for_service(service_id)
                .expect("service added to ctx above");

            if verbose {
                print_domain_info(&service_name, &service_domains);
            }

            let mut vars = override_railway_vars(raw_vars, Some(&service_domains), ctx);

            if let Some(port) = internal_port {
                vars.insert("PORT".to_string(), port.to_string());
            }

            if ctx.https_enabled() {
                inject_mkcert_ca_vars(&mut vars);
            }

            if !use_tui {
                print_code_service_summary(
                    &service_name,
                    &dev_config.command,
                    &working_dir,
                    vars.len(),
                    internal_port,
                    proxy_port,
                    https_config,
                );
            }

            let (private_url, public_url) = match (internal_port, proxy_port) {
                (Some(port), Some(pport)) => {
                    let private = format!("http://localhost:{}", port);
                    let public = https_config.as_ref().map(|config| {
                        let slug = slugify(&service_name);
                        if config.use_port_443 {
                            format!("https://{}.{}", slug, config.base_domain)
                        } else {
                            format!("https://{}:{}", config.base_domain, pport)
                        }
                    });
                    (Some(private), public)
                }
                _ => (None, None),
            };

            let color = COLORS[i % 6];
            tui_services.push(tui::ServiceInfo {
                name: service_name.clone(),
                is_docker: false,
                color,
                var_count: vars.len(),
                private_url,
                public_url,
                command: Some(dev_config.command.clone()),
                image: None,
            });

            process_manager
                .spawn_service(
                    service_name,
                    &dev_config.command,
                    working_dir,
                    vars,
                    log_tx.clone(),
                )
                .await?;
        }

        drop(log_tx);

        let mut docker_service_mapping = tui::ServiceMapping::new();
        for (i, summary) in service_summaries.iter().enumerate() {
            let color = COLORS[(tui_services.len() + i) % 6];
            let slug = slugify(&summary.name);
            docker_service_mapping.insert(slug.clone(), (summary.name.clone(), color));

            let (private_url, public_url) = summary
                .ports
                .iter()
                .find(|p| matches!(p.port_type, PortType::Http))
                .map(|p| {
                    let private = format!("http://localhost:{}", p.external);
                    let public = https_config.as_ref().map(|config| {
                        if config.use_port_443 {
                            format!("https://{}.{}", slug, config.base_domain)
                        } else {
                            format!("https://{}:{}", config.base_domain, p.public_port)
                        }
                    });
                    (Some(private), public)
                })
                .unwrap_or((None, None));

            tui_services.push(tui::ServiceInfo {
                name: summary.name.clone(),
                is_docker: true,
                color,
                var_count: summary.var_count,
                private_url,
                public_url,
                command: None,
                image: Some(summary.image.clone()),
            });
        }

        let (docker_tx, docker_rx) = mpsc::channel::<LogLine>(100);
        if has_image_services {
            let _ = tui::spawn_docker_logs(&output_path, docker_service_mapping, docker_tx).await;
        } else {
            drop(docker_tx);
        }

        Ok(Self {
            process_manager,
            tui_services,
            log_rx,
            docker_rx,
            output_path,
            has_image_services,
            code_count: configured_code_services.len(),
            image_count: service_summaries.len(),
            _session_lock: session_lock,
        })
    }

    pub async fn run(&mut self, use_tui: bool) -> Result<()> {
        if !use_tui {
            println!("{}", "Streaming logs (Ctrl+C to stop)...".dimmed());
            println!();

            loop {
                tokio::select! {
                    Some(log) = self.log_rx.recv() => {
                        print_log_line(&log);
                    }
                    _ = tokio::signal::ctrl_c() => {
                        eprintln!("\n{}", "Shutting down...".yellow());
                        break;
                    }
                }
            }
        } else {
            self.tui_services.sort_by_key(|s| s.private_url.is_none());
            let log_rx = std::mem::replace(&mut self.log_rx, mpsc::channel(1).1);
            let docker_rx = std::mem::replace(&mut self.docker_rx, mpsc::channel(1).1);
            let tui_services = std::mem::take(&mut self.tui_services);
            tui::run(log_rx, docker_rx, tui_services).await?;
        }

        if use_tui {
            print!("\x1b[2J\x1b[H");
            let _ = std::io::stdout().flush();
        }

        Ok(())
    }

    pub async fn shutdown(&mut self) {
        println!("{}", "Stopping services...".dimmed());

        self.process_manager.shutdown().await;
        if self.code_count > 0 {
            println!(
                " {} Stopped {} code service{}",
                "✓".green(),
                self.code_count,
                if self.code_count == 1 { "" } else { "s" }
            );
        }

        if self.has_image_services {
            let _ = tokio::process::Command::new("docker")
                .args([
                    "compose",
                    "-f",
                    &*self.output_path.to_string_lossy(),
                    "down",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
            println!(
                " {} Stopped {} image service{}",
                "✓".green(),
                self.image_count,
                if self.image_count == 1 { "" } else { "s" }
            );
        }

        println!();
        println!("{}", "All services stopped".green());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_acquire_lock() {
        let temp = TempDir::new().unwrap();
        let lock = DevelopSessionLock::try_acquire_at(temp.path());
        assert!(lock.is_ok());
    }

    #[test]
    fn test_concurrent_lock_fails() {
        let temp = TempDir::new().unwrap();
        let _lock1 = DevelopSessionLock::try_acquire_at(temp.path()).unwrap();
        let lock2 = DevelopSessionLock::try_acquire_at(temp.path());
        match lock2 {
            Ok(_) => panic!("should fail to acquire lock"),
            Err(e) => assert!(e.to_string().contains("Another develop session")),
        }
    }

    #[test]
    fn test_lock_released_on_drop() {
        let temp = TempDir::new().unwrap();
        {
            let _lock = DevelopSessionLock::try_acquire_at(temp.path()).unwrap();
        }
        // Lock should be released after drop
        let lock2 = DevelopSessionLock::try_acquire_at(temp.path());
        assert!(lock2.is_ok());
    }
}
