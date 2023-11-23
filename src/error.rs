use http::header::InvalidHeaderValue;
use thiserror::Error;

use crate::message::AzureCognitiveSpeechServicesMessageError;

#[derive(Debug, Error)]
pub enum Error {
	#[error("bad endpoint URL: {0}")]
	BadEndpointUrl(#[from] url::ParseError),
	#[error("invalid key: {0}")]
	InvalidKey(#[from] InvalidHeaderValue),
	#[error("websocket error: {0}")]
	Tungstenite(#[from] tokio_tungstenite::tungstenite::Error),
	#[error("I/O error: {0}")]
	Io(#[from] std::io::Error),
	#[error("error parsing/building message: {0}")]
	Message(#[from] AzureCognitiveSpeechServicesMessageError),
	#[error("error serializing SSML: {0}")]
	Ssml(#[from] ssml::Error),
	#[error("expected `{0}` event to have a binary body")]
	ExpectedBinary(&'static str),
	#[error("missing `{0}` field in {0}")]
	MissingField(&'static str, &'static str),
	#[error("failed to deserialize: {0}")]
	Deserialize(#[from] simd_json::Error),
	#[error("unexpected multiple streams in request")]
	UnexpectedMultipleStreams
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
