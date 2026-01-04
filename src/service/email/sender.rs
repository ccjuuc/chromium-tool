use lettre::message::{Mailbox, Message, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{SmtpTransport, Transport};
use crate::config::AppConfig;
use crate::model::build::BuildRequest;
use anyhow::{Context, Result};

#[derive(Clone)]
pub struct EmailSender {
    pub(crate) config: AppConfig,
}

impl EmailSender {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }
    
    pub async fn send_notification(
        &self,
        task_id: i64,
        request: &BuildRequest,
        additional_emails: Option<&str>,
    ) -> Result<()> {
        let email_config = &self.config.email;
        
        let mut email_to: Vec<Mailbox> = Vec::new();
        
        // 添加请求中的邮箱
        if let Some(emails) = additional_emails {
            email_to.extend(emails.split(',').filter_map(|s| {
                let s = s.trim();
                s.parse::<Mailbox>().ok()
            }));
        }
        
        // 添加配置中的邮箱
        email_to.extend(email_config.to.iter().filter_map(|s| {
            s.trim().parse::<Mailbox>().ok()
        }));
        
        if email_to.is_empty() {
            tracing::warn!("No valid recipients found, skipping email notification");
            return Ok(());
        }
        
        let web = &self.config.server.db_server;
        let data = serde_json::json!({
            "task_id": task_id,
            "branch": request.branch,
            "oem_name": request.oem_name,
            "platform": request.platform,
            "server": request.server,
            "pkg_flag": request.pkg_flag,
            "link": format!("http://{}", web),
        });
        
        let from_address = email_config.from
            .parse()
            .context("Invalid from address")?;
        
        let mut email_builder = Message::builder()
            .from(from_address)
            .subject(format!("{} Build Task", request.platform));
        
        for recipient in &email_to {
            email_builder = email_builder.to(recipient.clone());
        }
        
        let email_content = serde_json::to_string_pretty(&data)
            .context("Failed to serialize email content")?;
        
        let email = email_builder
            .singlepart(SinglePart::plain(email_content))
            .context("Failed to build email")?;
        
        let creds = Credentials::new(
            email_config.from.clone(),
            email_config.password.clone(),
        );
        
        let mailer = SmtpTransport::relay(&email_config.smtp)
            .context("Failed to create SMTP transport")?
            .credentials(creds)
            .build();
        
        mailer
            .send(&email)
            .context("Failed to send email")?;
        
        Ok(())
    }
}

