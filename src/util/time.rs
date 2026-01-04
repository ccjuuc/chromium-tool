use time::OffsetDateTime;
use time::macros::format_description;
use anyhow::Result;

pub fn format_date_time() -> Result<String> {
    let now = OffsetDateTime::now_local()?;
    let format = format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    Ok(now.format(&format)?)
}

#[allow(dead_code)]
pub fn format_date_folder() -> Result<String> {
    let now = OffsetDateTime::now_local()?;
    let format = format_description!("[year]-[month]-[day]-[hour]-[minute]");
    Ok(now.format(&format)?)
}

