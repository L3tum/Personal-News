use anyhow;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::SmtpTransport;
use lettre::{Message, Tokio1SmtpTransport};
use std::collections::HashMap;

use crate::config::SmtpConfig;

pub struct EmailClient {
    transporter: Tokio1SmtpTransport,
    config: SmtpConfig,
}

impl EmailClient {
    pub fn new(config: SmtpConfig) -> anyhow::Result<Self> {
        let creds = Credentials::new(
            config.username.clone(),
            config.password.clone(),
        );

        let transporter = SmtpTransport::builder_dangerous(&config.host)
            .port(config.port)
            .credentials(creds)
            .build::<Tokio1SmtpTransport>();

        Ok(Self { transporter, config })
    }

    /// Send a daily digest email to multiple recipients
    pub async fn send_digest(
        &self,
        subject: &str,
        body_markdown: &str,
        recipients: &[String],
    ) -> anyhow::Result<()> {
        // Convert markdown to HTML using a simple conversion
        let body_html = markdown_to_html(body_markdown);

        let mut message = Message::builder()
            .from(self.config.from.parse()?)
            .subject(subject)
            .header(ContentType::TEXT_HTML)
            .body(body_html);

        for recipient in recipients {
            let email = message.clone().to(recipient.parse()?);
            self.transporter.send(email).await?;
            log::info!("Sent digest to {}", recipient);
        }

        Ok(())
    }
}

/// Simple markdown to HTML converter (inline)
fn markdown_to_html(markdown: &str) -> String {
    let html = markdown
        // Code blocks
        .replace("```", "<pre><code>")
        .replace("```\n", "</code></pre>\n")
        // Headers
        .replace("### ", "<h3>")
        .replace("## ", "<h2>")
        .replace("# ", "<h1>")
        // Bold
        .replace("**", "<strong>")
        .replace("**", "</strong>")
        // Italic
        .replace("*", "<em>")
        .replace("*", "</em>")
        // Links [text](url)
        .replace(|s: &str| format!("[{}](", s), "<a href=\"")
        .replace(")", "\">")
        .replace("]", "</a>")
        // Line breaks
        .replace("\n\n", "</p><p>")
        .replace("\n", "<br>");

    // Wrap everything in paragraphs
    let mut wrapped = String::from("<p>");
    wrapped.push_str(&html);
    wrapped.push_str("</p>");

    wrapped
}
