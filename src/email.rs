use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::Tls;
use lettre::transport::smtp::SmtpTransport;
use lettre::{Message, Transport};
use pulldown_cmark::{html, Options, Parser};

use crate::config::SmtpConfig;

pub struct EmailClient {
    transporter: SmtpTransport,
    config: SmtpConfig,
}

impl EmailClient {
    pub fn new(config: SmtpConfig) -> anyhow::Result<Self> {
        let creds = Credentials::new(config.username.clone(), config.password.clone());

        use lettre::transport::smtp::client::TlsParameters;
        let tls_params = TlsParameters::builder(config.host.clone()).build()?;
        let tls_mode = match config.tls_mode.as_str() {
            "starttls" => Tls::Opportunistic(tls_params.clone()),
            "tls" => Tls::Required(tls_params),
            _ => Tls::None,
        };

        let transporter = SmtpTransport::builder_dangerous(&config.host)
            .port(config.port)
            .credentials(creds)
            .tls(tls_mode)
            .build();

        Ok(Self {
            transporter,
            config,
        })
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

        for recipient in recipients {
            let email = Message::builder()
                .from(self.config.from.parse()?)
                .to(recipient.parse()?)
                .subject(subject)
                .header(ContentType::TEXT_HTML)
                .body(body_html.clone())?;
            // Spawn blocking send to avoid blocking the async runtime
            let transporter = self.transporter.clone();
            let recipient = recipient.clone();
            let result = tokio::task::spawn_blocking(move || transporter.send(&email)).await?;
            result.map_err(|e| anyhow::anyhow!("SMTP error: {}", e))?;
            log::info!("Sent digest to {}", recipient);
        }

        Ok(())
    }
}

fn markdown_to_html(markdown: &str) -> String {
    let options = Options::all();
    let parser = Parser::new_ext(markdown, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}
