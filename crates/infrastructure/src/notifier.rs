use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::message::Message;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use tracing::warn;

use domain::errors::NotifyError;
use domain::ports::Notifier;

/// Строит `AsyncSmtpTransport` под конкретный `port`.
///
/// Общая логика для `SmtpNotifier` и `SmtpHealthCheck`: оба используют
/// один и тот же транспорт с одинаковыми правилами TLS.
///
/// ## Режимы TLS
///
/// - `465` — implicit TLS (SMTPS): TLS-handshake сразу после TCP-connect.
///   Yandex, Mail.ru, большинство managed SMTP.
/// - `587`, `25` — STARTTLS: сначала plaintext, потом upgrade через
///   команду `STARTTLS`. Использует `AsyncSmtpTransport::relay`, который
///   настраивает `Tls::Required`.
///
/// ## Ошибки
///
/// `NotifyError::Smtp` — не удалось построить TLS-контекст
/// (`TlsParameters::new` вызывает резолв DNS + построение
/// `ClientConfig`). Это **ошибка конфигурации**, fail-fast на bootstrap.
pub(crate) fn build_smtp_transport(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, NotifyError> {
    let builder = match port {
        465 => {
            // Implicit TLS: свой TLS-слой поверх соединения.
            let tls = TlsParameters::new(host.to_string())
                .map_err(|e| NotifyError::Smtp(format!("tls params: {e}")))?;
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
                .port(port)
                .tls(Tls::Wrapper(tls))
        }
        _ => {
            // STARTTLS (587, 25): `relay` настраивает Tls::Required.
            AsyncSmtpTransport::<Tokio1Executor>::relay(host)
                .map_err(|e| NotifyError::Smtp(e.to_string()))?
                .port(port)
        }
    };

    Ok(builder
        .credentials(Credentials::new(user.to_string(), password.to_string()))
        .build())
}

pub struct SmtpNotifier {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: String,
    to: Vec<String>,
}

impl SmtpNotifier {
    /// Создаёт notifier. Возвращает `Err(NotifyError::Smtp)` при
    /// невозможности построить TLS-контекст. На bootstrap воркера
    /// это fail-fast — лучше упасть, чем отправлять письма в пустоту.
    pub fn new(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        from: String,
        to: Vec<String>,
    ) -> Result<Self, NotifyError> {
        let transport = build_smtp_transport(host, port, user, password)?;
        Ok(Self {
            transport,
            from,
            to,
        })
    }
}

#[async_trait]
impl Notifier for SmtpNotifier {
    async fn send(&self, severity: &str, subject: &str, body: &str) -> Result<(), NotifyError> {
        let mut builder = Message::builder()
            .from(
                self.from
                    .parse()
                    .map_err(|e: lettre::address::AddressError| NotifyError::Smtp(e.to_string()))?,
            )
            .subject(format!("[{}] {}", severity.to_uppercase(), subject))
            .header(ContentType::TEXT_PLAIN);

        for to in &self.to {
            builder = builder.to(to
                .parse()
                .map_err(|e: lettre::address::AddressError| NotifyError::Smtp(e.to_string()))?);
        }

        let email = builder
            .body(body.to_string())
            .map_err(|e| NotifyError::Smtp(e.to_string()))?;

        self.transport.send(email).await.map(|_| ()).map_err(|e| {
            warn!(error = %e, "smtp send failed");
            NotifyError::Smtp(e.to_string())
        })
    }
}

pub struct NoopNotifier;

#[async_trait]
impl Notifier for NoopNotifier {
    async fn send(&self, severity: &str, subject: &str, _body: &str) -> Result<(), NotifyError> {
        tracing::info!(severity, subject, "noop notification");
        Ok(())
    }
}
