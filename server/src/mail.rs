use std::sync::{Arc, Mutex};

use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::db::Error;

#[derive(Clone, Debug)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

#[derive(Clone)]
pub enum Mailer {
    /// Sends through an SMTP server.
    Smtp { transport: AsyncSmtpTransport<Tokio1Executor>, from: Mailbox },
    /// Logs each message instead of sending it, for development without an SMTP server.
    Log,
    /// Keeps the messages in memory, for tests.
    Memory(Arc<Mutex<Vec<Mail>>>),
}

impl Mailer {
    /// SMTP when `SET_SMTP_URL` is set (it then needs `SET_MAIL_FROM`), otherwise `Log`.
    pub fn from_env() -> Result<Mailer, Error> {
        let Ok(url) = std::env::var("SET_SMTP_URL") else { return Ok(Mailer::Log) };
        let from = std::env::var("SET_MAIL_FROM").map_err(|_| "SET_SMTP_URL needs SET_MAIL_FROM")?.parse()?;
        Ok(Mailer::Smtp { transport: AsyncSmtpTransport::<Tokio1Executor>::from_url(&url)?.build(), from })
    }

    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<(), Error> {
        match self {
            Mailer::Smtp { transport, from } => {
                let message = Message::builder()
                    .from(from.clone())
                    .to(to.parse()?)
                    .subject(subject)
                    .body(body)?;
                transport.send(message).await?;
            }
            Mailer::Log => tracing::info!("Email to {to}: {subject}\n{body}"),
            Mailer::Memory(outbox) => {
                outbox.lock().unwrap().push(Mail { to: to.into(), subject: subject.into(), body });
            }
        }
        Ok(())
    }
}
