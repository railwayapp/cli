use super::*;

const DOCS_URL: &str = "https://docs.railway.com";
const LLMS_TXT_URL: &str = "https://docs.railway.com/llms.txt";
const LLMS_FULL_URL: &str = "https://docs.railway.com/llms-full.txt";

/// Open Railway Documentation in default browser, or print doc URLs in non-interactive mode
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args) -> Result<()> {
    if !crate::macros::is_stdout_terminal() {
        println!("{DOCS_URL}");
        println!("{LLMS_TXT_URL}");
        println!("{LLMS_FULL_URL}");
        return Ok(());
    }

    ::open::that(DOCS_URL)?;
    Ok(())
}
