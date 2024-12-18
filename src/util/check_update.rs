use colored::Colorize;

pub async fn check_update(configs: &mut crate::Configs, force: bool) -> anyhow::Result<bool> {
    let result = configs.check_update(force).await;
    if let Ok(Some(latest_version)) = result {
        println!(
            "{} v{} visit {} for more info",
            "New version available:".green().bold(),
            latest_version.yellow(),
            "https://docs.railway.com/guides/cli".purple(),
        );
        Ok(false)
    } else {
        Ok(true)
    }
}

pub async fn check_update_command(configs: &mut crate::Configs) -> anyhow::Result<()> {
    check_update(configs, false).await?;
    Ok(())
}
