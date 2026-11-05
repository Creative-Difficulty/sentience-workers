//! Error classification.
//!
//! The retry logic needs to know whether a failure says something about the
//! *message* being processed (bad LLM JSON, a request the provider rejects,
//! data Postgres refuses) or about the *system* (provider down, rate limited,
//! database unreachable). Only the former should burn the message's retry
//! budget unconditionally; see [`crate::retry::RetryTracker`].

use async_openai::error::{ApiError, OpenAIError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Caused by this message / batch; retrying the same input will most
    /// likely fail again.
    Message,
    /// Caused by the environment (LLM provider, network, database). Retrying
    /// later is expected to succeed.
    Systemic,
}

/// An error together with its classification.
#[derive(Debug)]
pub struct Failure {
    pub kind: FailureKind,
    pub error: color_eyre::Report,
}

impl Failure {
    pub fn message(error: impl Into<color_eyre::Report>) -> Self {
        Failure {
            kind: FailureKind::Message,
            error: error.into(),
        }
    }

    pub fn systemic(error: impl Into<color_eyre::Report>) -> Self {
        Failure {
            kind: FailureKind::Systemic,
            error: error.into(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} failure: {}", self.kind, self.error)
    }
}

impl From<sqlx::Error> for Failure {
    fn from(e: sqlx::Error) -> Self {
        Failure {
            kind: classify_sqlx(&e),
            error: e.into(),
        }
    }
}

impl From<OpenAIError> for Failure {
    fn from(e: OpenAIError) -> Self {
        Failure {
            kind: classify_openai(&e),
            error: e.into(),
        }
    }
}

/// Postgres data exceptions (SQLSTATE class 22, e.g. a NUL byte in text or an
/// out-of-range timestamp) and integrity violations (class 23) are caused by
/// the row being written; everything else (connection loss, pool timeout,
/// serialization failures, ...) is systemic.
pub fn classify_sqlx(e: &sqlx::Error) -> FailureKind {
    match e {
        sqlx::Error::Database(db) => match db.code() {
            Some(code) if code.starts_with("22") || code.starts_with("23") => FailureKind::Message,
            _ => FailureKind::Systemic,
        },
        sqlx::Error::RowNotFound | sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => {
            FailureKind::Message
        }
        _ => FailureKind::Systemic,
    }
}

/// async-openai does not expose the HTTP status, so this works on the error
/// shape: server errors (5xx) arrive as an `ApiError` without type/code,
/// 4xx carry the provider's `type`/`code`.
pub fn classify_openai(e: &OpenAIError) -> FailureKind {
    match e {
        // Connection refused, DNS, TLS, timeouts, reset: the provider or the
        // network, not this message. (Timeouts are ambiguous; they are treated
        // as systemic and only count against a message when the system was
        // otherwise healthy, see RetryTracker.)
        OpenAIError::Reqwest(_) => FailureKind::Systemic,
        OpenAIError::ApiError(api) => classify_api_error(api),
        // The envelope (not the model's content) could not be decoded: an
        // HTML error page from a proxy, a provider outage, ...
        OpenAIError::JSONDeserialize(..) => FailureKind::Systemic,
        // The request could not even be built from this input.
        OpenAIError::InvalidArgument(_) => FailureKind::Message,
        _ => FailureKind::Systemic,
    }
}

fn classify_api_error(api: &ApiError) -> FailureKind {
    if api.r#type.is_none() && api.code.is_none() {
        // 5xx (async-openai drops the body structure for server errors)
        return FailureKind::Systemic;
    }
    let tag = format!(
        "{} {}",
        api.r#type.as_deref().unwrap_or_default(),
        api.code.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();
    const SYSTEMIC: &[&str] = &[
        "rate_limit",
        "quota",
        "billing",
        "auth",
        "permission",
        "invalid_api_key",
        "server_error",
        "overloaded",
        "unavailable",
        "timeout",
        "model_not_found",
    ];
    if SYSTEMIC.iter().any(|s| tag.contains(s)) {
        FailureKind::Systemic
    } else {
        // invalid_request_error, context_length_exceeded, content_filter, ...
        FailureKind::Message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(t: Option<&str>, c: Option<&str>) -> OpenAIError {
        OpenAIError::ApiError(ApiError {
            message: "x".into(),
            r#type: t.map(Into::into),
            param: None,
            code: c.map(Into::into),
        })
    }

    #[test]
    fn openai_errors() {
        use FailureKind::*;
        assert_eq!(classify_openai(&api(None, None)), Systemic);
        assert_eq!(
            classify_openai(&api(Some("rate_limit_error"), None)),
            Systemic
        );
        assert_eq!(
            classify_openai(&api(Some("insufficient_quota"), Some("insufficient_quota"))),
            Systemic
        );
        assert_eq!(
            classify_openai(&api(Some("invalid_request_error"), Some("invalid_api_key"))),
            Systemic
        );
        assert_eq!(
            classify_openai(&api(
                Some("invalid_request_error"),
                Some("context_length_exceeded")
            )),
            Message
        );
        assert_eq!(
            classify_openai(&api(Some("invalid_request_error"), None)),
            Message
        );
        assert_eq!(
            classify_openai(&OpenAIError::InvalidArgument("bad".into())),
            Message
        );
        let je = serde_json::from_str::<u8>("x").unwrap_err();
        assert_eq!(
            classify_openai(&OpenAIError::JSONDeserialize(je, "<html>".into())),
            Systemic
        );
    }

    #[derive(Debug)]
    struct FakeDbError(Option<&'static str>);

    impl std::fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "fake db error {:?}", self.0)
        }
    }

    impl std::error::Error for FakeDbError {}

    impl sqlx::error::DatabaseError for FakeDbError {
        fn message(&self) -> &str {
            "fake"
        }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            self.0.map(std::borrow::Cow::Borrowed)
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn db(code: Option<&'static str>) -> sqlx::Error {
        sqlx::Error::Database(Box::new(FakeDbError(code)))
    }

    #[test]
    fn sqlx_errors() {
        use FailureKind::*;
        assert_eq!(classify_sqlx(&sqlx::Error::PoolTimedOut), Systemic);
        assert_eq!(classify_sqlx(&sqlx::Error::RowNotFound), Message);
        // 22021: invalid byte sequence (NUL in text); 22008: datetime out of range
        assert_eq!(classify_sqlx(&db(Some("22021"))), Message);
        assert_eq!(classify_sqlx(&db(Some("22008"))), Message);
        // 23505: unique violation; 23503: FK violation
        assert_eq!(classify_sqlx(&db(Some("23505"))), Message);
        assert_eq!(classify_sqlx(&db(Some("23503"))), Message);
        // 40001: serialization failure; 57P01: admin shutdown; 53300: too many connections
        assert_eq!(classify_sqlx(&db(Some("40001"))), Systemic);
        assert_eq!(classify_sqlx(&db(Some("57P01"))), Systemic);
        assert_eq!(classify_sqlx(&db(Some("53300"))), Systemic);
        assert_eq!(classify_sqlx(&db(Some("2"))), Systemic);
        assert_eq!(classify_sqlx(&db(None)), Systemic);
        assert_eq!(Failure::from(db(Some("22021"))).kind, Message);
    }

    #[test]
    fn display_names_kind_and_error() {
        let f = Failure::systemic(color_eyre::eyre::eyre!("provider down"));
        assert_eq!(f.to_string(), "Systemic failure: provider down");
        let f = Failure::message(color_eyre::eyre::eyre!("bad json"));
        assert_eq!(f.to_string(), "Message failure: bad json");
    }
}
