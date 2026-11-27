/// A call that gave no data. Its words are what the model reads: the API's own, or what to send instead, so that it
/// can call again. A tool gives it as an error result (`isError`, its words as text), never as a protocol error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Arguments no call can take, like an empty slug or a limit that is no number: what to send instead.
    #[error("{0}")]
    Advice(String),
    /// A 4xx answer, with the API's words, and what to send instead where the tool knows it.
    #[error("{message}")]
    Api {
        /// Its HTTP status.
        status: u16,
        /// Its words.
        message: String,
    },
    /// Another answer than 200 or a 4xx.
    #[error("Datacircle's API answered {0}: try again later")]
    Status(u16),
    /// No answer in time.
    #[error("Datacircle's API did not answer in time: try again later")]
    Timeout,
    /// The API could not be reached.
    #[error("Datacircle's API could not be reached: {0}")]
    Network(String),
    /// An answer that is no JSON of the API's.
    #[error("Datacircle's API answered no JSON: {0}")]
    Decode(String),
}

impl Error {
    pub(crate) fn from_reqwest(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else {
            Self::Network(error.without_url().to_string())
        }
    }

    /// The HTTP status the API answered, where it answered one.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } | Self::Status(status) => Some(*status),
            _ => None,
        }
    }

    /// Whether the same call may work later: a 429, a 5xx, no answer in time or no answer.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api { status, .. } => *status == 429,
            Self::Status(status) => *status >= 500,
            Self::Timeout | Self::Network(_) => true,
            Self::Advice(_) | Self::Decode(_) => false,
        }
    }
}
